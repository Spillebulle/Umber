//! What a layer-effect bake costs, at canvas resolution.
//!
//! `docs/layer-effects.md` §3.4 holds the figures this produces and the four
//! readings taken from them; **re-run this before quoting any of them**, which
//! is the rule `measure-clipboard.rs` records having learned when numbers three
//! times too slow got written into the docs by a machine that was busy.
//!
//! It was written to settle one question — whether a canvas-sized bake fits
//! inside a frame — and it settled two more that had been got wrong by
//! reasoning: that a full-resolution tent blur costs the same at every radius
//! (it does not), and that the shadow would be the expensive effect (the stroke
//! is).
//!
//! Run it:
//!
//! ```sh
//! cargo run --release -p umber-render --example measure-effects
//! cargo run --release -p umber-render --example measure-effects -- 2048 4096
//! ```
//!
//! **The shaders here are prototypes and deliberately not in `shaders/`.** They
//! exist to make the measurement honest about pass count and tap count, not to
//! be the production bake; keeping them out of the shader directory is what
//! stops one being adopted by accident.
//!
//! # What is measured, and what that is worth
//!
//! Wall-clock around `submit` plus a blocking `poll`, median of several runs
//! after a warm-up. That is **not** a GPU timestamp: `Features::TIMESTAMP_QUERY`
//! is not among the features Umber requests, and asking for it here would
//! measure a device Umber never creates. What it does measure is the thing the
//! design actually needs to know — how much wall time a bake adds to a frame
//! that has to wait for it — and it is an over-estimate by whatever the submit
//! and fence cost, which is the safe direction.
//!
//! Three bakes are timed, at each canvas size and each radius:
//!
//! - **shadow, full resolution** — the naive reading of the design's §3.2: a
//!   tent blur as two box passes per axis, every pass at canvas resolution.
//! - **shadow, quarter resolution** — the same tent, with the blur done on a
//!   4x downsample and bilinearly upsampled. 16x fewer texels and a quarter the
//!   radius, so ~64x less blur work, at a quality cost that only shows on a
//!   hard edge.
//! - **stroke, jump flood** — the signed distance field of §3.1, `log2(r) + 1`
//!   passes ping-ponging a seed coordinate.
//!
//! # The thing this was written to check
//!
//! §3.2 of the design said the tent blur was "linear in the area whatever the
//! radius", borrowing the claim from `umber-core::selection`'s feather. **That
//! is a property of running sums on the CPU and it does not survive the move to
//! a fragment shader**, which has no running sum: a box pass there is `2r + 1`
//! taps per texel.
//!
//! It matters. At 10000² a full-resolution tent went from 8.5 ms at radius 4 to
//! 83 ms at radius 64 — ten times, against a claim that predicted no change at
//! all. The design has been corrected and now blurs on a 4x downsample, which
//! is 2.0 to 3.2 ms across the same sweep. Borrowing an algorithm's complexity
//! across a change of execution model is the mistake worth remembering; the
//! kernel carried over and the bound did not.

use std::time::Instant;

use umber_render::gpu::Gpu;

/// Canvas sizes measured when none are given on the command line.
///
/// The second is `max_texture_dimension_2d`'s neighbourhood and the size the
/// undo, autosave and readback arguments are all stated against, so it is the
/// one the design's claims have to survive.
const DEFAULT_SIZES: [u32; 2] = [2048, 10000];

/// Effect radii measured. A 4 px stroke, a 16 px shadow and a 64 px glow are
/// the small, ordinary and generous cases.
const RADII: [u32; 3] = [4, 16, 64];

/// Timed runs per bake, after the warm-up. The median is reported.
const RUNS: usize = 7;
const WARMUP: usize = 3;

/// A frame at 60 Hz. The bake has to fit inside what is left of one after the
/// composite, the interface and the dab pass have taken their share, so fitting
/// this is necessary and a good way short of sufficient.
const FRAME_60HZ_MS: f64 = 1000.0 / 60.0;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // The machine this was written on has an RTX 3080, which is the best case
    // and therefore the misleading one: Umber asks for `downlevel_defaults`
    // precisely so a desktop build cannot depend on what a weak GPU refuses.
    // `--fallback` runs the whole sweep on the software rasteriser instead,
    // which brackets the answer from below. Neither end is an integrated or a
    // mobile GPU; between them is where one lands.
    let choice = if args.iter().any(|a| a == "--fallback") {
        umber_render::gpu::Choice::Fallback
    } else {
        umber_render::gpu::Choice::Best
    };

    let sizes: Vec<u32> = {
        let given: Vec<u32> = args
            .iter()
            .filter(|a| !a.starts_with("--"))
            .filter_map(|a| a.parse().ok())
            .filter(|n| *n > 0)
            .collect();
        if given.is_empty() {
            DEFAULT_SIZES.to_vec()
        } else {
            given
        }
    };
    if sizes.is_empty() {
        eprintln!("no usable canvas sizes given");
        std::process::exit(2);
    }

    let gpu = pollster::block_on(Gpu::with_adapter(Gpu::create_instance(), None, choice))
        .unwrap_or_else(|e| panic!("{e}"));
    let info = gpu.adapter.get_info();
    println!(
        "adapter: {} ({:?}, {:?})",
        info.name, info.device_type, info.backend
    );
    println!(
        "max texture dimension: {}",
        gpu.adapter.limits().max_texture_dimension_2d
    );

    // A measuring tool must not die on the first allocation a device refuses:
    // "10000 square does not fit" is a *result*, and wgpu's default handler
    // would turn it into a panic with the rest of the run unreported.
    gpu.device.on_uncaptured_error(std::sync::Arc::new(|e| {
        eprintln!("  !! device error: {e}");
    }));

    let jfa_ok = supports_jfa(&gpu);
    if !jfa_ok {
        println!("note: Rg16Uint is not a render target here; the stroke bake is skipped");
    }
    println!();

    let bench = Bench::new(&gpu);

    for size in sizes {
        let max = gpu.adapter.limits().max_texture_dimension_2d;
        if size > max {
            println!("== {size} x {size} ==  skipped: over max_texture_dimension_2d ({max})\n");
            continue;
        }
        println!("== {size} x {size} ==");
        println!("  {:.1} Mtexel", (size as f64 * size as f64) / 1.0e6);

        report_footprint(size, jfa_ok);

        let Some(sh) = Shadow::new(&gpu, &bench, size) else {
            println!("  shadow: could not allocate\n");
            continue;
        };

        println!();
        println!(
            "  {:<10} {:>12} {:>12} {:>12}",
            "radius", "shadow/full", "shadow/quarter", "stroke/jfa"
        );

        for r in RADII {
            let full = sh.time_full(&gpu, r);
            let quarter = sh.time_quarter(&gpu, r);
            let stroke = if jfa_ok {
                sh.time_stroke(&gpu, r)
            } else {
                f64::NAN
            };
            println!(
                "  {:<10} {:>12} {:>12} {:>12}",
                format!("{r} px"),
                ms(full),
                ms(quarter),
                ms(stroke),
            );
        }
        drop(sh);
        println!();
    }

    println!("verdict is per row: a bake fits a 60 Hz frame under {FRAME_60HZ_MS:.1} ms,");
    println!("and has to share that frame with the composite, the dab pass and the interface.");
}

fn ms(v: f64) -> String {
    if v.is_nan() {
        "-".into()
    } else if v >= FRAME_60HZ_MS {
        format!("{v:.2} !")
    } else {
        format!("{v:.2}")
    }
}

/// What the bakes hold at once, so a refusal is explained before it happens.
fn report_footprint(size: u32, jfa: bool) {
    let texels = size as f64 * size as f64;
    let mb = |bytes_per_texel: f64| texels * bytes_per_texel / (1024.0 * 1024.0);
    // layer (4) + coverage (1) + two blur scratches (1 each) + output (4).
    let shadow = mb(4.0 + 1.0 + 1.0 + 1.0 + 4.0);
    // layer + coverage + two Rg16Uint seed buffers (4 each) + output.
    let stroke = mb(4.0 + 1.0 + 4.0 + 4.0 + 4.0);
    print!("  textures: shadow {shadow:.0} MB");
    if jfa {
        print!(", stroke {stroke:.0} MB");
    }
    println!();
}

/// Does this device take `Rg16Uint` as a colour attachment?
///
/// The jump flood stores a seed *coordinate*, which has to be exact. Sixteen
/// bits of unsigned integer covers any canvas Umber allows; an `f16` would not,
/// since its mantissa runs out of exact integers at 2048 — which is a canvas
/// size Umber ships with. Asked rather than assumed, because the whole point of
/// `downlevel_defaults` is that a desktop build must not depend on what a
/// mobile GPU refuses.
fn supports_jfa(gpu: &Gpu) -> bool {
    gpu.adapter
        .get_texture_format_features(wgpu::TextureFormat::Rg16Uint)
        .allowed_usages
        .contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
}

// ---------------------------------------------------------------------------
// Uniform
// ---------------------------------------------------------------------------

/// Mirrors `Cfg` in the WGSL below. Laid out so the two agree byte for byte
/// without a `vec3` anywhere near it — see CLAUDE.md's "Uniform layout".
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct Cfg {
    /// Texel offset per tap, in texels: `(1, 0)` or `(0, 1)`.
    step: [f32; 2],
    /// Size of the *target*, in texels.
    size: [f32; 2],
    /// Effect colour, premultiplied linear.
    tint: [f32; 4],
    /// Box-blur half-width, in texels of the target.
    radius: i32,
    /// Jump-flood step, in texels.
    k: i32,
    /// Stroke half-width, in texels.
    width: f32,
    _pad: f32,
}

// SAFETY: `#[repr(C)]`, no padding beyond the explicit `_pad`, all members are
// plain old data.
unsafe impl bytemuck::Zeroable for Cfg {}
unsafe impl bytemuck::Pod for Cfg {}

const CFG_SIZE: u64 = std::mem::size_of::<Cfg>() as u64;

// ---------------------------------------------------------------------------
// Shaders
// ---------------------------------------------------------------------------

/// Everything but the jump flood: one oversized triangle, `textureLoad`
/// throughout.
///
/// `textureLoad` rather than a sampler almost everywhere, deliberately. A blur
/// tap wants the texel and not a filtered neighbourhood, and taking the sampler
/// out means one bind-group layout instead of two and no argument about
/// filtering support on the uint path. The upsample is the exception and does
/// its bilinear by hand, four loads and two mixes, which is what a production
/// version would let the sampler do — so the tap count is honest and the cost
/// is if anything slightly over-stated.
const FLOAT_WGSL: &str = r#"
struct Cfg {
    step: vec2<f32>,
    size: vec2<f32>,
    tint: vec4<f32>,
    radius: i32,
    k: i32,
    width: f32,
    pad: f32,
};

@group(0) @binding(0) var<uniform> c: Cfg;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var aux: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0),
    );
    return vec4<f32>(p[vi], 0.0, 1.0);
}

fn at(t: texture_2d<f32>, p: vec2<i32>, lim: vec2<i32>) -> vec4<f32> {
    return textureLoad(t, clamp(p, vec2<i32>(0), lim - vec2<i32>(1)), 0);
}

// The layer's alpha is the coverage every effect derives from. A real bake
// multiplies the mask in here too; that is one more load and does not move a
// measurement.
@fragment
fn fs_extract(@builtin(position) f: vec4<f32>) -> @location(0) f32 {
    let lim = vec2<i32>(c.size);
    return at(src, vec2<i32>(f.xy), lim).a;
}

// One box pass. Two of these per axis make a tent, which is the kernel
// `umber-core::selection`'s feather uses and the reason to match it is that a
// shadow and a feather of the same radius should fall off the same way.
//
// `2r + 1` taps. On the CPU the feather pays O(1) per texel through a running
// sum; a fragment shader has no such thing, which is the point this example
// exists to price.
@fragment
fn fs_box(@builtin(position) f: vec4<f32>) -> @location(0) f32 {
    let lim = vec2<i32>(c.size);
    let p = vec2<i32>(f.xy);
    let d = vec2<i32>(c.step);
    var sum = 0.0;
    for (var i = -c.radius; i <= c.radius; i = i + 1) {
        sum = sum + at(src, p + d * i, lim).r;
    }
    return sum / f32(2 * c.radius + 1);
}

// 4x box downsample: sixteen loads, once, so the blur that follows runs on a
// sixteenth of the texels at a quarter of the radius.
@fragment
fn fs_down(@builtin(position) f: vec4<f32>) -> @location(0) f32 {
    let lim = vec2<i32>(c.size) * 4;
    let p = vec2<i32>(f.xy) * 4;
    var sum = 0.0;
    for (var y = 0; y < 4; y = y + 1) {
        for (var x = 0; x < 4; x = x + 1) {
            sum = sum + at(src, p + vec2<i32>(x, y), lim).r;
        }
    }
    return sum / 16.0;
}

// The shadow's last pass: upsample, tint, and knock the layer's own coverage
// out of it.
//
// The knockout is baked rather than composited — design §3.3. It is the
// reason `aux` is bound: an outer effect needs the layer's coverage at the
// same time as its own.
@fragment
fn fs_tint_up(@builtin(position) f: vec4<f32>) -> @location(0) vec4<f32> {
    let lim = vec2<i32>(c.size);
    let small = vec2<i32>(c.size) / 4;
    let uv = (f.xy - vec2<f32>(0.5)) * 0.25;
    let base = vec2<i32>(floor(uv));
    let frac = fract(uv);
    let a00 = at(src, base + vec2<i32>(0, 0), small).r;
    let a10 = at(src, base + vec2<i32>(1, 0), small).r;
    let a01 = at(src, base + vec2<i32>(0, 1), small).r;
    let a11 = at(src, base + vec2<i32>(1, 1), small).r;
    let a = mix(mix(a00, a10, frac.x), mix(a01, a11, frac.x), frac.y);
    let cov = at(aux, vec2<i32>(f.xy), lim).r;
    return c.tint * a * (1.0 - cov);
}

// The same last pass without the upsample, for the full-resolution bake.
@fragment
fn fs_tint(@builtin(position) f: vec4<f32>) -> @location(0) vec4<f32> {
    let lim = vec2<i32>(c.size);
    let a = at(src, vec2<i32>(f.xy), lim).r;
    let cov = at(aux, vec2<i32>(f.xy), lim).r;
    return c.tint * a * (1.0 - cov);
}
"#;

/// The jump flood: seed, `log2(r)` halving steps, then resolve to a stroke.
const JFA_WGSL: &str = r#"
struct Cfg {
    step: vec2<f32>,
    size: vec2<f32>,
    tint: vec4<f32>,
    radius: i32,
    k: i32,
    width: f32,
    pad: f32,
};

@group(0) @binding(0) var<uniform> c: Cfg;
@group(0) @binding(1) var seeds: texture_2d<u32>;
@group(0) @binding(2) var cov: texture_2d<f32>;

const NONE: u32 = 65535u;

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0),
    );
    return vec4<f32>(p[vi], 0.0, 1.0);
}

// A texel inside the shape seeds itself; everything else starts unclaimed.
// The distance field this grows is the distance to the *edge* of the coverage,
// which is what a stroke width and a shadow spread are both measured in.
@fragment
fn fs_seed(@builtin(position) f: vec4<f32>) -> @location(0) vec2<u32> {
    let lim = vec2<i32>(c.size) - vec2<i32>(1);
    let p = vec2<i32>(f.xy);
    let a = textureLoad(cov, clamp(p, vec2<i32>(0), lim), 0).r;
    if (a > 0.5) {
        return vec2<u32>(u32(p.x), u32(p.y));
    }
    return vec2<u32>(NONE, NONE);
}

// One flood step. Nine candidates at +-k; keep the nearest seed.
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

// Distance to the shape, turned into an outside stroke of half-width
// `c.width`, antialiased over one texel and knocked out under the layer.
@fragment
fn fs_stroke(@builtin(position) f: vec4<f32>) -> @location(0) vec4<f32> {
    let lim = vec2<i32>(c.size) - vec2<i32>(1);
    let p = vec2<i32>(f.xy);
    let s = textureLoad(seeds, clamp(p, vec2<i32>(0), lim), 0).xy;
    if (s.x == NONE) {
        return vec4<f32>(0.0);
    }
    let d = distance(vec2<f32>(p), vec2<f32>(vec2<i32>(s)));
    let a = 1.0 - smoothstep(c.width - 1.0, c.width, d);
    let inside = textureLoad(cov, clamp(p, vec2<i32>(0), lim), 0).r;
    return c.tint * a * (1.0 - inside);
}
"#;

// ---------------------------------------------------------------------------
// Pipelines
// ---------------------------------------------------------------------------

/// The pipelines, built once and shared by every canvas size.
struct Bench {
    float_bgl: wgpu::BindGroupLayout,
    jfa_bgl: wgpu::BindGroupLayout,
    extract: wgpu::RenderPipeline,
    box_blur: wgpu::RenderPipeline,
    down: wgpu::RenderPipeline,
    tint: wgpu::RenderPipeline,
    tint_up: wgpu::RenderPipeline,
    seed: wgpu::RenderPipeline,
    flood: wgpu::RenderPipeline,
    stroke: wgpu::RenderPipeline,
}

const R8: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;
const RGBA: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const SEED: wgpu::TextureFormat = wgpu::TextureFormat::Rg16Uint;

impl Bench {
    fn new(gpu: &Gpu) -> Self {
        let d = &gpu.device;

        let entry = |binding: u32, ty: wgpu::BindingType| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty,
            count: None,
        };
        let uniform = wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: wgpu::BufferSize::new(CFG_SIZE),
        };
        let tex = |sample_type| wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        };
        let float_tex = tex(wgpu::TextureSampleType::Float { filterable: false });
        let uint_tex = tex(wgpu::TextureSampleType::Uint);

        let float_bgl = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("effects-float"),
            entries: &[entry(0, uniform), entry(1, float_tex), entry(2, float_tex)],
        });
        let jfa_bgl = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("effects-jfa"),
            entries: &[entry(0, uniform), entry(1, uint_tex), entry(2, float_tex)],
        });

        let float_pl = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("effects-float"),
            bind_group_layouts: &[Some(&float_bgl)],
            immediate_size: 0,
        });
        let jfa_pl = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("effects-jfa"),
            bind_group_layouts: &[Some(&jfa_bgl)],
            immediate_size: 0,
        });

        let float_sh = d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("effects-float"),
            source: wgpu::ShaderSource::Wgsl(FLOAT_WGSL.into()),
        });
        let jfa_sh = d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("effects-jfa"),
            source: wgpu::ShaderSource::Wgsl(JFA_WGSL.into()),
        });

        let make = |label: &str,
                    module: &wgpu::ShaderModule,
                    layout: &wgpu::PipelineLayout,
                    fs: &str,
                    format: wgpu::TextureFormat| {
            d.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(format.into())],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };

        Self {
            extract: make("extract", &float_sh, &float_pl, "fs_extract", R8),
            box_blur: make("box", &float_sh, &float_pl, "fs_box", R8),
            down: make("down", &float_sh, &float_pl, "fs_down", R8),
            tint: make("tint", &float_sh, &float_pl, "fs_tint", RGBA),
            tint_up: make("tint-up", &float_sh, &float_pl, "fs_tint_up", RGBA),
            seed: make("seed", &jfa_sh, &jfa_pl, "fs_seed", SEED),
            flood: make("flood", &jfa_sh, &jfa_pl, "fs_step", SEED),
            stroke: make("stroke", &jfa_sh, &jfa_pl, "fs_stroke", RGBA),
            float_bgl,
            jfa_bgl,
        }
    }
}

// ---------------------------------------------------------------------------
// Targets
// ---------------------------------------------------------------------------

fn texture(gpu: &Gpu, label: &str, size: u32, format: wgpu::TextureFormat) -> wgpu::TextureView {
    gpu.device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&Default::default())
}

/// One canvas size's worth of targets, plus the timing.
struct Shadow<'a> {
    size: u32,
    layer: wgpu::TextureView,
    cov: wgpu::TextureView,
    a: wgpu::TextureView,
    b: wgpu::TextureView,
    small_a: wgpu::TextureView,
    small_b: wgpu::TextureView,
    out: wgpu::TextureView,
    seed_a: wgpu::TextureView,
    seed_b: wgpu::TextureView,
    bench: &'a Bench,
}

impl<'a> Shadow<'a> {
    fn new(gpu: &Gpu, bench: &'a Bench, size: u32) -> Option<Self> {
        let small = (size / 4).max(1);
        Some(Self {
            size,
            layer: texture(gpu, "layer", size, RGBA),
            cov: texture(gpu, "coverage", size, R8),
            a: texture(gpu, "blur-a", size, R8),
            b: texture(gpu, "blur-b", size, R8),
            small_a: texture(gpu, "small-a", small, R8),
            small_b: texture(gpu, "small-b", small, R8),
            out: texture(gpu, "effect", size, RGBA),
            seed_a: texture(gpu, "seed-a", size, SEED),
            seed_b: texture(gpu, "seed-b", size, SEED),
            bench,
        })
    }

    /// Six full-resolution passes: extract, four box passes, tint.
    fn time_full(&self, gpu: &Gpu, radius: u32) -> f64 {
        let r = radius as i32;
        time(gpu, |enc| {
            self.pass(
                gpu,
                enc,
                Pipe::Extract,
                &self.layer,
                &self.layer,
                &self.cov,
                self.size,
                cfg_blur(self.size, 0, 0),
            );
            self.pass(
                gpu,
                enc,
                Pipe::Box,
                &self.cov,
                &self.cov,
                &self.a,
                self.size,
                cfg_blur(self.size, r, 0),
            );
            self.pass(
                gpu,
                enc,
                Pipe::Box,
                &self.a,
                &self.a,
                &self.b,
                self.size,
                cfg_blur(self.size, r, 0),
            );
            self.pass(
                gpu,
                enc,
                Pipe::Box,
                &self.b,
                &self.b,
                &self.a,
                self.size,
                cfg_blur(self.size, r, 1),
            );
            self.pass(
                gpu,
                enc,
                Pipe::Box,
                &self.a,
                &self.a,
                &self.b,
                self.size,
                cfg_blur(self.size, r, 1),
            );
            self.pass(
                gpu,
                enc,
                Pipe::Tint,
                &self.b,
                &self.cov,
                &self.out,
                self.size,
                cfg_blur(self.size, 0, 0),
            );
        })
    }

    /// Extract and tint at full resolution, the four box passes on a quarter.
    fn time_quarter(&self, gpu: &Gpu, radius: u32) -> f64 {
        let small = (self.size / 4).max(1);
        let r = ((radius / 4) as i32).max(1);
        time(gpu, |enc| {
            self.pass(
                gpu,
                enc,
                Pipe::Extract,
                &self.layer,
                &self.layer,
                &self.cov,
                self.size,
                cfg_blur(self.size, 0, 0),
            );
            self.pass(
                gpu,
                enc,
                Pipe::Down,
                &self.cov,
                &self.cov,
                &self.small_a,
                small,
                cfg_blur(small, 0, 0),
            );
            self.pass(
                gpu,
                enc,
                Pipe::Box,
                &self.small_a,
                &self.small_a,
                &self.small_b,
                small,
                cfg_blur(small, r, 0),
            );
            self.pass(
                gpu,
                enc,
                Pipe::Box,
                &self.small_b,
                &self.small_b,
                &self.small_a,
                small,
                cfg_blur(small, r, 0),
            );
            self.pass(
                gpu,
                enc,
                Pipe::Box,
                &self.small_a,
                &self.small_a,
                &self.small_b,
                small,
                cfg_blur(small, r, 1),
            );
            self.pass(
                gpu,
                enc,
                Pipe::Box,
                &self.small_b,
                &self.small_b,
                &self.small_a,
                small,
                cfg_blur(small, r, 1),
            );
            self.pass(
                gpu,
                enc,
                Pipe::TintUp,
                &self.small_a,
                &self.cov,
                &self.out,
                self.size,
                cfg_blur(self.size, 0, 0),
            );
        })
    }

    /// Extract, seed, `log2(r)` flood steps, resolve.
    fn time_stroke(&self, gpu: &Gpu, radius: u32) -> f64 {
        let steps = (32 - radius.max(1).leading_zeros()).max(1);
        time(gpu, |enc| {
            self.pass(
                gpu,
                enc,
                Pipe::Extract,
                &self.layer,
                &self.layer,
                &self.cov,
                self.size,
                cfg_blur(self.size, 0, 0),
            );
            self.pass(
                gpu,
                enc,
                Pipe::Seed,
                &self.seed_b,
                &self.cov,
                &self.seed_a,
                self.size,
                cfg_blur(self.size, 0, 0),
            );
            let mut from_a = true;
            for i in 0..steps {
                let k = 1i32 << (steps - 1 - i);
                let (src, dst) = if from_a {
                    (&self.seed_a, &self.seed_b)
                } else {
                    (&self.seed_b, &self.seed_a)
                };
                let mut c = cfg_blur(self.size, 0, 0);
                c.k = k;
                self.pass(gpu, enc, Pipe::Flood, src, &self.cov, dst, self.size, c);
                from_a = !from_a;
            }
            let last = if from_a { &self.seed_a } else { &self.seed_b };
            let mut c = cfg_blur(self.size, 0, 0);
            c.width = radius as f32;
            self.pass(
                gpu,
                enc,
                Pipe::Stroke,
                last,
                &self.cov,
                &self.out,
                self.size,
                c,
            );
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn pass(
        &self,
        gpu: &Gpu,
        enc: &mut wgpu::CommandEncoder,
        pipe: Pipe,
        src: &wgpu::TextureView,
        aux: &wgpu::TextureView,
        dst: &wgpu::TextureView,
        size: u32,
        mut cfg: Cfg,
    ) {
        cfg.size = [size as f32, size as f32];
        // One uniform buffer reused by every pass in the encoder would hand
        // them all the *last* value written: `write_buffer` is ordered against
        // the submission, not against the passes inside it. A buffer per pass
        // is the cheap correct thing in a measurement.
        let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cfg"),
            size: CFG_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        gpu.queue.write_buffer(&buf, 0, bytemuck::bytes_of(&cfg));

        let b = self.bench;
        let (layout, pipeline) = match pipe {
            Pipe::Extract => (&b.float_bgl, &b.extract),
            Pipe::Box => (&b.float_bgl, &b.box_blur),
            Pipe::Down => (&b.float_bgl, &b.down),
            Pipe::Tint => (&b.float_bgl, &b.tint),
            Pipe::TintUp => (&b.float_bgl, &b.tint_up),
            Pipe::Seed => (&b.jfa_bgl, &b.seed),
            Pipe::Flood => (&b.jfa_bgl, &b.flood),
            Pipe::Stroke => (&b.jfa_bgl, &b.stroke),
        };

        let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(src),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(aux),
                },
            ],
        });

        let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: dst,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        rp.set_pipeline(pipeline);
        rp.set_bind_group(0, &bg, &[]);
        rp.draw(0..3, 0..1);
    }
}

#[derive(Clone, Copy)]
enum Pipe {
    Extract,
    Box,
    Down,
    Tint,
    TintUp,
    Seed,
    Flood,
    Stroke,
}

fn cfg_blur(size: u32, radius: i32, axis: u32) -> Cfg {
    Cfg {
        step: if axis == 0 { [1.0, 0.0] } else { [0.0, 1.0] },
        size: [size as f32, size as f32],
        // An ordinary shadow: black at 60%, premultiplied.
        tint: [0.0, 0.0, 0.0, 0.6],
        radius,
        k: 0,
        width: 0.0,
        _pad: 0.0,
    }
}

/// Median wall-clock over [`RUNS`] submissions, after [`WARMUP`].
///
/// The median rather than the mean, because one scheduling hiccup on a machine
/// running anything else at all would otherwise decide the number — which is
/// the mistake `examples/measure-clipboard.rs` records having made once
/// already, when figures three times too slow got written into the docs.
fn time(gpu: &Gpu, mut record: impl FnMut(&mut wgpu::CommandEncoder)) -> f64 {
    let run = |record: &mut dyn FnMut(&mut wgpu::CommandEncoder)| {
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        record(&mut enc);
        let t = Instant::now();
        gpu.queue.submit([enc.finish()]);
        let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
        t.elapsed().as_secs_f64() * 1000.0
    };

    for _ in 0..WARMUP {
        run(&mut record);
    }
    let mut times: Vec<f64> = (0..RUNS).map(|_| run(&mut record)).collect();
    times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    times[times.len() / 2]
}
