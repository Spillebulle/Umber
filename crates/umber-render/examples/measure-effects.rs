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
//! It has since been asked a second question — **how a stroke should get its
//! distance from the layer's alpha edge**, jump flooding or blur-and-threshold
//! — and that one cannot be settled by a table. So it also draws the two
//! methods over a shape built to discriminate between them and writes the
//! pictures out, because a 20 px stroke by each method settles the argument
//! faster than the argument does.
//!
//! Run it:
//!
//! ```sh
//! cargo run --release -p umber-render --example measure-effects
//! cargo run --release -p umber-render --example measure-effects -- 2048 4096
//! cargo run --release -p umber-render --example measure-effects -- --pictures-only
//! cargo run --release -p umber-render --example measure-effects -- --filled
//! ```
//!
//! `--no-pictures` skips the drawing, `--pictures-only` skips the sweep,
//! `--pictures <dir>` puts them somewhere other than `dist/effect-stroke`, and
//! `--fallback` runs the whole thing on the software rasteriser.
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
//! Five bakes are timed, at each canvas size and each radius:
//!
//! - **shadow, full resolution** — the naive reading of the design's §3.2: a
//!   tent blur as two box passes per axis, every pass at canvas resolution.
//! - **shadow, quarter resolution** — the same tent, with the blur done on a
//!   4x downsample and bilinearly upsampled. 16x fewer texels and a quarter the
//!   radius, so ~64x less blur work, at a quality cost that only shows on a
//!   hard edge.
//! - **stroke, jump flood** — the signed distance field of §3.1, `log2(r) + 1`
//!   passes ping-ponging a seed coordinate.
//! - **stroke, blur and threshold, full resolution** — the same tent as the
//!   shadow, resolved into a distance rather than tinted directly. A stroke has
//!   a hard edge, which is the one thing §3.2 says must not be downsampled, so
//!   this is the column the method has to be judged on.
//! - **stroke, blur and threshold, quarter resolution** — the version that
//!   really does reuse the shadow's own blur, priced so the quality cost in the
//!   pictures can be weighed against what it saves.
//!
//! **The fourth column is the one the design was missing**, and it is the
//! reason the cost argument in §13 comes out the other way round: that argument
//! compares a *quarter-resolution* blur against a *full-resolution* flood, and
//! the two do not do the same job. A box blur is `O(r)` per axis and a jump
//! flood is `O(log r)`, so at a large radius on a large canvas the blur is the
//! slower of the two, not the faster.
//!
//! # The pictures
//!
//! One shape, drawn at 768 square, carrying the places the two methods can
//! disagree: a sharp convex corner, an acute apex, a diagonal edge, a limb
//! narrower than the stroke, and holes smaller than it. A picture without those
//! proves nothing, because on a straight axis-aligned edge the two methods are
//! *identical by construction* — see `blur_distance` below.
//!
//! **The widths are corrected before anything is compared**, and without that
//! the comparison would be void. A threshold at 0.5 of a blurred coverage field
//! is the original edge, not a grown one; the amount a lower threshold grows by
//! depends on the kernel. So the blur path does not threshold at all — it
//! *inverts* the tent kernel's own cumulative distribution to recover a
//! distance, and then hands that to the same softening the flood's own resolve
//! uses. Both methods therefore mean the same thing by "20 px", exactly, on an
//! axis-aligned edge; everywhere else the difference is the measurement.
//!
//! **And the kernel width is swept rather than chosen**, which is the other way
//! this could have been a rigged test. How good blur-and-threshold is turns
//! almost entirely on one number — the box radius as a multiple of the stroke's
//! width — and the trade runs the opposite way to the intuition: a *tighter*
//! kernel is better, and cheaper. At `h = 2w` a 20 px stroke puts a
//! right-angled corner 8.6 px out where a disc puts it at 20; at `h = 1.25w`
//! the same corner reaches 16.6 and the thin limb, which had no stroke at all,
//! comes back exactly right. Reporting the loose setting alone would have been
//! a straw man, and the first draft of this measurement was one.
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
    if !supports_wide(&gpu) {
        println!("note: R16Float is not a render target here; the blur stroke is skipped");
    }
    let filled = args.iter().any(|a| a == "--filled");
    if filled {
        println!("note: --filled puts a shape in the layer before the sweep");
    }
    println!();

    let bench = Bench::new(&gpu);

    if !args.iter().any(|a| a == "--no-pictures") {
        let dir = picture_dir(&args);
        pictures::draw(&gpu, &bench, &dir, jfa_ok && supports_wide(&gpu));
    }
    if args.iter().any(|a| a == "--pictures-only") {
        return;
    }

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
        if filled {
            sh.fill_shape(&gpu);
        }

        println!();
        println!(
            "  {:<10} {:>12} {:>15} {:>12} {:>14} {:>17}",
            "radius",
            "shadow/full",
            "shadow/quarter",
            "stroke/jfa",
            "stroke/blur",
            "stroke/blur-qtr",
        );

        let wide_ok = supports_wide(&gpu);
        for r in RADII {
            let full = sh.time_full(&gpu, r);
            let quarter = sh.time_quarter(&gpu, r);
            let stroke = if jfa_ok {
                sh.time_stroke(&gpu, r)
            } else {
                f64::NAN
            };
            let (blur, blur_q) = if wide_ok {
                (
                    sh.time_stroke_blur(&gpu, r),
                    sh.time_stroke_blur_quarter(&gpu, r),
                )
            } else {
                (f64::NAN, f64::NAN)
            };
            println!(
                "  {:<10} {:>12} {:>15} {:>12} {:>14} {:>17}",
                format!("{r} px"),
                ms(full),
                ms(quarter),
                ms(stroke),
                ms(blur),
                ms(blur_q),
            );
        }
        drop(sh);
        println!();
        // After the prototypes and on the same canvas, so the two tables can be
        // read against each other. Dropped `sh` first: the shipped bake allocates
        // a working set of its own and holding both at 10000² is over two
        // gigabytes for no reason.
        bake_sweep(&gpu, size);
    }

    println!("verdict is per row: a bake fits a 60 Hz frame under {FRAME_60HZ_MS:.1} ms,");
    println!("and has to share that frame with the composite, the dab pass and the interface.");
}

/// What the **shipped** bake costs, as against the prototypes above.
///
/// The sweep above prices *methods* and is what settled which method to write;
/// this prices `CanvasRenderer::bake_effects`, which is the thing a frame
/// actually waits for. They are not the same measurement and quoting one for the
/// other is the mistake §3.1a records — the rows here are labelled with the
/// column of the table above they correspond to, so the two can be compared
/// rather than confused.
///
/// Three shapes, because they are the three pass structures the bake has:
///
/// - **a shadow at its default settings** — no spread, so no jump flood at all,
///   which is the case every application opens a drop shadow at and is the one
///   the design did not price separately;
/// - **a wide shadow** — the downsampled tent, `EFFECT_FULL_RES_SOFTNESS` and
///   above;
/// - **an outline** — the flood, which is the expensive half of the feature.
///
/// The colour is nudged between runs. That is not decoration: the cache is keyed
/// on a hash of the parameters, so a second bake of the same effect is a
/// comparison and nothing else, and the sweep would otherwise report the cost of
/// deciding there was nothing to do. Colour is the one parameter that changes the
/// hash and changes no pass.
fn bake_sweep(gpu: &Gpu, size: u32) {
    use umber_core::{Effect, OutlinePosition, PixelRect};
    use umber_render::{CanvasRenderer, EffectFrame, LayerDraw, LayerEffects, StrokeStyle};

    let mut canvas = CanvasRenderer::new(
        &gpu.device,
        glam::UVec2::splat(size),
        wgpu::TextureFormat::Rgba8Unorm,
    );
    // Something for the flood to find. A band rather than the whole canvas, so
    // the flood's inner branch is taken — §3.4's own caveat is that a
    // zero-initialised layer prices the cheapest content there is.
    let band = PixelRect {
        x: size / 4,
        y: size / 4,
        width: size / 2,
        height: size / 2,
    };
    let rgba: Vec<u8> = std::iter::repeat_n([255u8, 255, 255, 255], (band.area()) as usize)
        .flatten()
        .collect();
    canvas.write_layer_rect(&gpu.queue, 0, band, &rgba);

    let draw = LayerDraw {
        slot: 0,
        opacity: 1.0,
        blend: 0,
        visible: true,
        mask: None,
        clipped: false,
    };
    let frame = EffectFrame {
        active_index: u32::MAX,
        stroke: StrokeStyle {
            opacity: 0.0,
            ..Default::default()
        },
        stroke_live: false,
    };

    let cases: [(&str, &str, Effect); 4] = [
        (
            "shadow, default (softness 5)",
            "shadow/full @ 4",
            Effect::drop_shadow(),
        ),
        (
            "shadow, softness 64",
            "shadow/quarter @ 64",
            Effect {
                softness: 64.0,
                ..Effect::drop_shadow()
            },
        ),
        (
            "outline 16, outside",
            "stroke/jfa @ 16",
            Effect {
                spread: 16.0,
                position: OutlinePosition::Outside,
                ..Effect::outline()
            },
        ),
        // **The expensive one, and the reason it is here.** A centred outline
        // straddles the edge, so it needs the outward distance *and* the inward
        // one and floods twice — which makes it the worst bake there is and the
        // figure `EFFECT_MAX_PASSES_PER_EFFECT` is sized against. Nothing priced
        // it until the knockout became per-kind and made the position real.
        (
            "outline 16, centre",
            "stroke/jfa @ 16, twice",
            Effect {
                spread: 16.0,
                position: OutlinePosition::Centre,
                ..Effect::outline()
            },
        ),
    ];

    println!("  {:<30} {:>10}   prototype column", "shipped bake", "ms");
    for (what, like, effect) in cases {
        let mut nudge = 0.0f32;
        let t = time(gpu, |enc| {
            nudge += 1.0 / 4096.0;
            let effects = [Effect {
                color: umber_core::Color::new(0.0, nudge, 0.0, 1.0),
                ..effect
            }];
            let stack = [LayerEffects {
                draw,
                effects: &effects,
            }];
            canvas.bake_effects(&gpu.device, &gpu.queue, enc, 2, &stack, frame);
        });
        println!("  {what:<30} {:>10}   {like}", ms(t));
    }
    println!();
}

/// Where the pictures go. `dist/` is in `.gitignore`, so a run leaves the tree
/// clean — the code that draws them is committed and the pictures are not.
fn picture_dir(args: &[String]) -> std::path::PathBuf {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--pictures"
            && let Some(p) = it.next()
        {
            return std::path::PathBuf::from(p);
        }
    }
    std::path::PathBuf::from("dist/effect-stroke")
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
    // layer + coverage + two R16Float blur scratches (2 each) + output.
    let blur = mb(4.0 + 1.0 + 2.0 + 2.0 + 4.0);
    print!("  textures: shadow {shadow:.0} MB");
    if jfa {
        print!(", stroke/jfa {stroke:.0} MB");
    }
    println!(", stroke/blur {blur:.0} MB");
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

/// Does this device take `R16Float` as a colour attachment?
///
/// Asked for the same reason, and expected to be yes everywhere: CLAUDE.md's
/// scratch-format argument already establishes that `R16Float` is
/// `(msaa_resolve, attachment)` on `Features::empty()`, where `R16Unorm` is
/// storage-only and cannot be a render target at all.
fn supports_wide(gpu: &Gpu) -> bool {
    gpu.adapter
        .get_texture_format_features(WIDE)
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
    /// How far the blur's kernel reaches, in texels of the *output*: the half
    /// support of the tent, `2 x radius`, or four times that where the blur ran
    /// on a quarter-resolution copy. It is what turns a blurred coverage back
    /// into a distance, so it has to be the kernel that was actually run rather
    /// than the one that was asked for — a radius clamped to 1 on the
    /// downsample is a wider reach than `2 x width` and the correction has to
    /// know.
    reach: f32,
}

// SAFETY: `#[repr(C)]`, the trailing `reach` keeps the size a multiple of 16
// with no implicit padding anywhere, and all members are plain old data.
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
    reach: f32,
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

// ---------------------------------------------------------------------------
// Blur and threshold, as a distance
// ---------------------------------------------------------------------------

// Recover an outward distance from a blurred coverage field.
//
// **This is the whole of what makes the comparison fair, and thresholding at
// 0.5 is what makes it void.** Blur a step edge with any symmetric kernel and
// the 0.5 contour is where the edge already was; a *lower* threshold grows the
// shape, by an amount that is a property of the kernel rather than a number
// anybody chose. So rather than pick a threshold and hope, invert the kernel.
//
// Two box passes per axis make a tent, so along the normal of a straight edge
// the blurred value is the tent's cumulative distribution. For a symmetric
// triangle of half support `h`, at signed distance `x` (positive inside),
//
//     b(x) = 0.5 * (1 + x/h)^2      for -h <= x <= 0
//
// which inverts to `x = h * (sqrt(2b) - 1)`, so the outward distance is
// `h * (1 - sqrt(2b))`. That is exact for a straight axis-aligned edge: the two
// methods agree there to the last bit, which is precisely why a picture of one
// proves nothing and the shape has to carry corners, diagonals and thin limbs.
//
// `min(b, 0.5)` clamps the inside to zero distance; an outer stroke is knocked
// out under the layer anyway.
fn blur_distance(b: f32) -> f32 {
    return c.reach * (1.0 - sqrt(2.0 * min(max(b, 0.0), 0.5)));
}

// Bilinear read of a quarter-resolution field.
//
// The four loads are written out again rather than shared with `fs_tint_up`,
// because that entry point is one §3.4's published figures were taken through
// and this file's first duty is that they still reproduce. A prototype may
// carry a duplicate that a shipped shader may not.
fn quarter(f: vec2<f32>, small: vec2<i32>) -> f32 {
    let uv = (f - vec2<f32>(0.5)) * 0.25;
    let base = vec2<i32>(floor(uv));
    let fr = fract(uv);
    let a00 = at(src, base + vec2<i32>(0, 0), small).r;
    let a10 = at(src, base + vec2<i32>(1, 0), small).r;
    let a01 = at(src, base + vec2<i32>(0, 1), small).r;
    let a11 = at(src, base + vec2<i32>(1, 1), small).r;
    return mix(mix(a00, a10, fr.x), mix(a01, a11, fr.x), fr.y);
}

// The stroke's last pass on the blur path, tinted — the one that is timed.
@fragment
fn fs_blur_stroke(@builtin(position) f: vec4<f32>) -> @location(0) vec4<f32> {
    let lim = vec2<i32>(c.size);
    let d = blur_distance(at(src, vec2<i32>(f.xy), lim).r);
    let a = 1.0 - smoothstep(c.width - 1.0, c.width, d);
    let cov = at(aux, vec2<i32>(f.xy), lim).r;
    return c.tint * a * (1.0 - cov);
}

// The same, reading a quarter-resolution blur.
@fragment
fn fs_blur_stroke_up(@builtin(position) f: vec4<f32>) -> @location(0) vec4<f32> {
    let lim = vec2<i32>(c.size);
    let d = blur_distance(quarter(f.xy, vec2<i32>(c.size) / 4));
    let a = 1.0 - smoothstep(c.width - 1.0, c.width, d);
    let cov = at(aux, vec2<i32>(f.xy), lim).r;
    return c.tint * a * (1.0 - cov);
}

// ---------------------------------------------------------------------------
// The picture probes
// ---------------------------------------------------------------------------

// The probes write the raw numbers rather than a picture: red is the stroke's
// own alpha after the knockout, green is the layer's coverage. The colours a
// human looks at are composed on the CPU from those two, so one readback serves
// both the picture and the measurements taken off it — and so the measurements
// are of the alpha the shader produced rather than of a colour ramp.
//
// The target is non-sRGB, so the byte read back is the value written.
//
// **The probes soften over `w +- 0.5` where the timed paths soften over
// `w - 1 .. w`**, and that is a measurement fix rather than a difference of
// opinion. `smoothstep(w - 1, w, d)` puts its own half-way point at `w - 0.5`,
// so a stroke asked for twenty draws its contour at 19.5 — half a pixel that
// the eye never sees and that would sit in every figure in the table below,
// looking like a property of the algorithm. Centring it makes the calibration
// column read the width that was asked for, on both methods.
fn edge(d: f32) -> f32 {
    return 1.0 - smoothstep(c.width - 0.5, c.width + 0.5, d);
}

@fragment
fn fs_blur_probe(@builtin(position) f: vec4<f32>) -> @location(0) vec4<f32> {
    let lim = vec2<i32>(c.size);
    let a = edge(blur_distance(at(src, vec2<i32>(f.xy), lim).r));
    let cov = at(aux, vec2<i32>(f.xy), lim).r;
    return vec4<f32>(a * (1.0 - cov), cov, 0.0, 1.0);
}

@fragment
fn fs_blur_probe_up(@builtin(position) f: vec4<f32>) -> @location(0) vec4<f32> {
    let lim = vec2<i32>(c.size);
    let a = edge(blur_distance(quarter(f.xy, vec2<i32>(c.size) / 4)));
    let cov = at(aux, vec2<i32>(f.xy), lim).r;
    return vec4<f32>(a * (1.0 - cov), cov, 0.0, 1.0);
}

// A shape for the layer, so a sweep can be run over something rather than over
// an empty canvas.
//
// It is `--filled`'s and not the default, because §3.4's published figures were
// taken with the layer left at its zero-initialised state and this file's first
// duty is that they still reproduce. What it buys is the honest reading: the
// flood's inner branch is never taken when there are no seeds at all, so the
// default sweep prices the cheapest content there is.
@fragment
fn fs_shape(@builtin(position) f: vec4<f32>) -> @location(0) vec4<f32> {
    let p = f.xy / c.size * 21.0;
    let v = sin(p.x) * sin(p.y) + 0.35 * sin(p.x * 3.3 + 1.0) - 0.15;
    if (v > 0.0) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    return vec4<f32>(0.0);
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
    reach: f32,
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

// The same resolve, writing the raw numbers for the picture pass, and with the
// one correction that makes it comparable with the blur path.
//
// **Half a texel.** A seed is the *centre* of a covered texel, so the distance
// to the nearest seed over-states the distance to the shape's edge by half a
// texel wherever that edge falls on a texel boundary — which is how the test
// shape is built, deliberately. The blur path recovers a distance from the
// 0.5-coverage contour, which is the edge itself. Left uncorrected, every
// difference picture would be dominated by a uniform half-pixel band and the
// four features it exists to show would be buried under it.
//
// It is not in `fs_stroke` above, because that is a timed path and §3.4's
// figures have to keep reproducing. A subtraction does not move a measurement,
// but "does not move it" is a claim and not having to make it is better.
@fragment
fn fs_stroke_probe(@builtin(position) f: vec4<f32>) -> @location(0) vec4<f32> {
    let lim = vec2<i32>(c.size) - vec2<i32>(1);
    let p = vec2<i32>(f.xy);
    let inside = textureLoad(cov, clamp(p, vec2<i32>(0), lim), 0).r;
    let s = textureLoad(seeds, clamp(p, vec2<i32>(0), lim), 0).xy;
    if (s.x == NONE) {
        return vec4<f32>(0.0, inside, 0.0, 1.0);
    }
    let d = distance(vec2<f32>(p), vec2<f32>(vec2<i32>(s))) - 0.5;
    let a = 1.0 - smoothstep(c.width - 0.5, c.width + 0.5, d);
    return vec4<f32>(a * (1.0 - inside), inside, 0.0, 1.0);
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
    box_wide: wgpu::RenderPipeline,
    down_wide: wgpu::RenderPipeline,
    blur_stroke: wgpu::RenderPipeline,
    blur_stroke_up: wgpu::RenderPipeline,
    stroke_probe: wgpu::RenderPipeline,
    blur_probe: wgpu::RenderPipeline,
    blur_probe_up: wgpu::RenderPipeline,
    shape: wgpu::RenderPipeline,
}

const R8: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;
const RGBA: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const SEED: wgpu::TextureFormat = wgpu::TextureFormat::Rg16Uint;

/// The blur intermediate on the *stroke's* path, where the shadow's is `R8`.
///
/// Not a free upgrade and not an arbitrary one. Recovering a distance from a
/// blurred field magnifies its quantisation by `dd/db = reach / sqrt(2b)`, and
/// at the contour of a stroke of width `w` blurred with `reach = 2w` that is
/// `4w` — so one level of an eight-bit field moves the stroke's own edge by
/// `4w/255`, which is a third of a pixel at 20 px and over a pixel at 64. That
/// is a wound the method does not have to carry, so the pictures show it
/// **both** ways and the timing prices the wider one. `R16Float` is
/// `(msaa_resolve, attachment)` on `Features::empty()`, so it is a render
/// target on the guaranteed set, unlike `R16Unorm`.
const WIDE: wgpu::TextureFormat = wgpu::TextureFormat::R16Float;

/// The picture probes' target. Non-sRGB, so the byte read back is the byte the
/// shader wrote and a measurement taken off it is a measurement of the shader.
const PROBE: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

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
            box_wide: make("box-wide", &float_sh, &float_pl, "fs_box", WIDE),
            down_wide: make("down-wide", &float_sh, &float_pl, "fs_down", WIDE),
            blur_stroke: make("blur-stroke", &float_sh, &float_pl, "fs_blur_stroke", RGBA),
            blur_stroke_up: make(
                "blur-stroke-up",
                &float_sh,
                &float_pl,
                "fs_blur_stroke_up",
                RGBA,
            ),
            stroke_probe: make("stroke-probe", &jfa_sh, &jfa_pl, "fs_stroke_probe", PROBE),
            blur_probe: make("blur-probe", &float_sh, &float_pl, "fs_blur_probe", PROBE),
            blur_probe_up: make(
                "blur-probe-up",
                &float_sh,
                &float_pl,
                "fs_blur_probe_up",
                PROBE,
            ),
            shape: make("shape", &float_sh, &float_pl, "fs_shape", RGBA),
            float_bgl,
            jfa_bgl,
        }
    }
}

// ---------------------------------------------------------------------------
// Targets
// ---------------------------------------------------------------------------

fn texture_of(
    gpu: &Gpu,
    label: &str,
    size: u32,
    format: wgpu::TextureFormat,
    extra: wgpu::TextureUsages,
) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
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
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | extra,
        view_formats: &[],
    })
}

fn texture(gpu: &Gpu, label: &str, size: u32, format: wgpu::TextureFormat) -> wgpu::TextureView {
    texture_of(gpu, label, size, format, wgpu::TextureUsages::empty())
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
    wide_a: wgpu::TextureView,
    wide_b: wgpu::TextureView,
    small_wide_a: wgpu::TextureView,
    small_wide_b: wgpu::TextureView,
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
            wide_a: texture(gpu, "wide-a", size, WIDE),
            wide_b: texture(gpu, "wide-b", size, WIDE),
            small_wide_a: texture(gpu, "small-wide-a", small, WIDE),
            small_wide_b: texture(gpu, "small-wide-b", small, WIDE),
            bench,
        })
    }

    /// Put a shape in the layer, for `--filled`.
    fn fill_shape(&self, gpu: &Gpu) {
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        // Bound to `cov` rather than to the layer: a colour attachment may not
        // also be sampled, even by a shader that never reads the binding.
        self.pass(
            gpu,
            &mut enc,
            Pipe::Shape,
            &self.cov,
            &self.cov,
            &self.layer,
            self.size,
            cfg_blur(self.size, 0, 0),
        );
        gpu.queue.submit([enc.finish()]);
        let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
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

    /// Extract, four full-resolution box passes, resolve to a distance.
    ///
    /// Exactly the shadow's full-resolution pass structure with a different
    /// last pass — which is the point: the blur is the same blur, so what this
    /// column prices is a stroke taking the path the design assumed was cheap
    /// because the shadow was already paying for it. It is not the same
    /// payment. The shadow may run its blur on a quarter-resolution copy and a
    /// stroke has a hard edge, which §3.2 names as the one thing that must not.
    fn time_stroke_blur(&self, gpu: &Gpu, width: u32) -> f64 {
        let r = blur_radius_for(width, BLUR_FACTOR);
        let cfg = |radius: i32, axis: u32| cfg_blur(self.size, radius, axis);
        let mut resolve = cfg(0, 0);
        resolve.width = width as f32;
        // `2r + 1` rather than `2r`, matching the picture path: the tent is two
        // *discrete* boxes of `2r + 1` taps, so its continuous half support is
        // a texel wider than twice the box radius. It moves no timing, and the
        // point of matching exactly is that "the stroke that was timed is the
        // stroke that was drawn" should not be something anybody has to check.
        resolve.reach = 2.0 * r as f32 + 1.0;
        time(gpu, |enc| {
            let steps: [(&wgpu::TextureView, &wgpu::TextureView, u32); 4] = [
                (&self.cov, &self.wide_a, 0),
                (&self.wide_a, &self.wide_b, 0),
                (&self.wide_b, &self.wide_a, 1),
                (&self.wide_a, &self.wide_b, 1),
            ];
            self.pass(
                gpu,
                enc,
                Pipe::Extract,
                &self.layer,
                &self.layer,
                &self.cov,
                self.size,
                cfg(0, 0),
            );
            for (src, dst, axis) in steps {
                self.pass(
                    gpu,
                    enc,
                    Pipe::BoxWide,
                    src,
                    src,
                    dst,
                    self.size,
                    cfg(r, axis),
                );
            }
            self.pass(
                gpu,
                enc,
                Pipe::BlurStroke,
                &self.wide_b,
                &self.cov,
                &self.out,
                self.size,
                resolve,
            );
        })
    }

    /// The same, with the blur on a quarter-resolution copy.
    fn time_stroke_blur_quarter(&self, gpu: &Gpu, width: u32) -> f64 {
        let small = (self.size / 4).max(1);
        let r = blur_radius_for(width, BLUR_FACTOR).div_euclid(4).max(1);
        let cfg = |size: u32, radius: i32, axis: u32| cfg_blur(size, radius, axis);
        let mut resolve = cfg(self.size, 0, 0);
        resolve.width = width as f32;
        // Four times the small-texel reach, because the distance recovered from
        // it is read in full-resolution texels.
        resolve.reach = 4.0 * (2.0 * r as f32 + 1.0);
        time(gpu, |enc| {
            let steps: [(&wgpu::TextureView, &wgpu::TextureView, u32); 4] = [
                (&self.small_wide_a, &self.small_wide_b, 0),
                (&self.small_wide_b, &self.small_wide_a, 0),
                (&self.small_wide_a, &self.small_wide_b, 1),
                (&self.small_wide_b, &self.small_wide_a, 1),
            ];
            self.pass(
                gpu,
                enc,
                Pipe::Extract,
                &self.layer,
                &self.layer,
                &self.cov,
                self.size,
                cfg(self.size, 0, 0),
            );
            self.pass(
                gpu,
                enc,
                Pipe::DownWide,
                &self.cov,
                &self.cov,
                &self.small_wide_a,
                small,
                cfg(small, 0, 0),
            );
            for (src, dst, axis) in steps {
                self.pass(
                    gpu,
                    enc,
                    Pipe::BoxWide,
                    src,
                    src,
                    dst,
                    small,
                    cfg(small, r, axis),
                );
            }
            self.pass(
                gpu,
                enc,
                Pipe::BlurStrokeUp,
                &self.small_wide_a,
                &self.cov,
                &self.out,
                self.size,
                resolve,
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
        cfg: Cfg,
    ) {
        record(gpu, self.bench, enc, pipe, src, aux, dst, size, cfg);
    }
}

/// One pass, recorded. A free function because the picture pass has its own
/// targets and none of `Shadow`'s.
#[allow(clippy::too_many_arguments)]
fn record(
    gpu: &Gpu,
    b: &Bench,
    enc: &mut wgpu::CommandEncoder,
    pipe: Pipe,
    src: &wgpu::TextureView,
    aux: &wgpu::TextureView,
    dst: &wgpu::TextureView,
    size: u32,
    mut cfg: Cfg,
) {
    cfg.size = [size as f32, size as f32];
    {
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

        let (layout, pipeline) = match pipe {
            Pipe::Extract => (&b.float_bgl, &b.extract),
            Pipe::Box => (&b.float_bgl, &b.box_blur),
            Pipe::Down => (&b.float_bgl, &b.down),
            Pipe::Tint => (&b.float_bgl, &b.tint),
            Pipe::TintUp => (&b.float_bgl, &b.tint_up),
            Pipe::Seed => (&b.jfa_bgl, &b.seed),
            Pipe::Flood => (&b.jfa_bgl, &b.flood),
            Pipe::Stroke => (&b.jfa_bgl, &b.stroke),
            Pipe::BoxWide => (&b.float_bgl, &b.box_wide),
            Pipe::DownWide => (&b.float_bgl, &b.down_wide),
            Pipe::BlurStroke => (&b.float_bgl, &b.blur_stroke),
            Pipe::BlurStrokeUp => (&b.float_bgl, &b.blur_stroke_up),
            Pipe::StrokeProbe => (&b.jfa_bgl, &b.stroke_probe),
            Pipe::BlurProbe => (&b.float_bgl, &b.blur_probe),
            Pipe::BlurProbeUp => (&b.float_bgl, &b.blur_probe_up),
            Pipe::Shape => (&b.float_bgl, &b.shape),
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
    BoxWide,
    DownWide,
    BlurStroke,
    BlurStrokeUp,
    StrokeProbe,
    BlurProbe,
    BlurProbeUp,
    Shape,
}

/// The box half-width a stroke of this width needs, in full-resolution texels,
/// as a multiple of the width.
///
/// **This one number decides how good blur-and-threshold is, and reporting the
/// method at one value of it would be a straw man.** Two box passes per axis
/// make a tent of half support `2r + 1`, and the recovered distance can never
/// exceed that, so `r` has to clear `w/2` or the stroke's own contour falls
/// outside the kernel and there is nothing to find. Above that floor the
/// trade runs the *opposite* way to the intuition: a **tighter** kernel is
/// better, because it puts the contour further into the tail where a corner's
/// `F(-d/sqrt2)^2` and an edge's `F(-d)` are closer together. The arithmetic,
/// for a tent of half support `h = rho * w`, is that a right-angled corner
/// reaches `sqrt2 * h * (1 - sqrt(sqrt2 * (1 - 1/rho)))` — 0.45w at `rho = 2`,
/// 0.83w at 1.25, and about 1.0w at 1.1. `pictures::draw` sweeps it rather
/// than trusting that.
///
/// The floor is `ceil(w/2)`, which is the smallest `r` with `2r + 1 > w`. Right
/// at it the contour sits in the last texel of the kernel's support, where the
/// field is a few thousandths and about to be nothing at all; the sweep runs
/// down to it deliberately, because that is where the arithmetic says the
/// corner comes out exact and it is worth seeing whether anything else breaks
/// on the way.
fn blur_radius_for(width: u32, factor: f32) -> i32 {
    let floor = (width as f32 / 2.0).ceil() as i32;
    ((width as f32 * factor).round() as i32).max(floor).max(1)
}

/// The factor the timed columns use.
///
/// **A minimum rather than an end of the range**, which matters because "the
/// tightest setting" would be a suspicious answer and this is not it. Below
/// about `h = 1.1w` the corner stops being cut and starts *bulging*: at
/// `h = 1.02w` a 64 px stroke puts a right angle 82.5 px out where a disc puts
/// it at 64.
///
/// It is what `pictures::radius_sweep` independently picks at all four widths
/// measured — `r` of 1, 4, 11 and 35 at widths 2, 8, 20 and 64 — judged on the
/// worst any one feature is out by rather than on a mean over the picture. So
/// the stroke that is timed here is the same stroke the pictures draw, and it
/// is the method at its best rather than at a setting chosen to flatter the
/// conclusion. Being also the cheapest of the useful settings, since a box pass
/// is `2r + 1` taps, the cost column is generous to it too.
const BLUR_FACTOR: f32 = 0.55;

fn cfg_blur(size: u32, radius: i32, axis: u32) -> Cfg {
    Cfg {
        step: if axis == 0 { [1.0, 0.0] } else { [0.0, 1.0] },
        size: [size as f32, size as f32],
        // An ordinary shadow: black at 60%, premultiplied.
        tint: [0.0, 0.0, 0.0, 0.6],
        radius,
        k: 0,
        width: 0.0,
        reach: 0.0,
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

// ---------------------------------------------------------------------------
// The pictures
// ---------------------------------------------------------------------------

/// A stroke drawn both ways over a shape built to tell them apart.
///
/// The design's §13 leaves one algorithm open — jump flooding against
/// blur-and-threshold — and says a picture settles it faster than the argument
/// does. This is that picture, with the numbers taken off the same readback so
/// nobody has to eyeball a corner and guess how many pixels it lost.
///
/// **What makes it a test rather than a demonstration** is that the shape
/// carries the places the two methods can disagree and that both are drawn at
/// the same *width*. On a straight axis-aligned edge they agree exactly, by
/// construction; a picture of one would show two identical strokes and prove
/// nothing at all.
mod pictures {
    use super::*;

    /// Big enough to hold a 64 px stroke around everything without the shape's
    /// features running into each other, small enough to look at.
    const SIZE: u32 = 768;

    /// The margin the shape sits inside.
    ///
    /// Not decoration. Every probe below marches outward from an edge until the
    /// stroke's contour crosses a half, and `Field::at` clamps at the canvas —
    /// so an edge with less clearance than the stroke is wide reads the clamped
    /// border column for ever and reports no contour at all. The first run of
    /// this measurement had a 40 px margin and every 64 px reading came back
    /// "none", which looks exactly like a stroke that failed to draw.
    const OFF: f32 = 120.0;

    /// The widths drawn.
    ///
    /// 20 is the one §13 asks for; 2 and 8 are where the method is expected to
    /// be fine and 64 is where it is expected not to be. **32 and 48 are here
    /// because the answer stage 1 needs is a number, and four points with a
    /// factor of three between the last two cannot give one** — the first
    /// version of this sweep could say only "fine at 20, wrong at 64", which
    /// is a range and not a threshold.
    const WIDTHS: [u32; 6] = [2, 8, 20, 32, 48, 64];

    /// A field of alpha in pixel-index space: the value at integer `(i, j)` is
    /// texel `(i, j)`, so a continuous coordinate `x` is at index `x - 0.5`.
    struct Field {
        a: Vec<f32>,
    }

    impl Field {
        fn get(&self, i: i32, j: i32) -> f32 {
            let i = i.clamp(0, SIZE as i32 - 1) as usize;
            let j = j.clamp(0, SIZE as i32 - 1) as usize;
            self.a[j * SIZE as usize + i]
        }

        fn at(&self, x: f32, y: f32) -> f32 {
            let (fi, fj) = (x.floor(), y.floor());
            let (fx, fy) = (x - fi, y - fj);
            let (i, j) = (fi as i32, fj as i32);
            let top = self.get(i, j) * (1.0 - fx) + self.get(i + 1, j) * fx;
            let bot = self.get(i, j + 1) * (1.0 - fx) + self.get(i + 1, j + 1) * fx;
            top * (1.0 - fy) + bot * fy
        }
    }

    // -----------------------------------------------------------------------
    // The shape
    // -----------------------------------------------------------------------

    /// Is this continuous point inside the shape?
    ///
    /// Five features, and every one of them is here because the two methods
    /// answer it differently:
    ///
    /// - **a sharp convex corner** — the body's four right angles. A true
    ///   distance field puts the stroke's contour a full width out along the
    ///   diagonal; a blurred field reads a quarter-plane as less coverage than
    ///   a half-plane and pulls the corner in.
    /// - **an acute apex** — the spike, at about thirteen degrees. The same
    ///   failure, as hard as a shape can make it.
    /// - **a diagonal edge** — the capsule's long sides. A tent from separable
    ///   box passes has *square* support, so its cumulative distribution along
    ///   a forty-five degree normal is not the one along an axis, and a
    ///   distance recovered from it is out by a fixed fraction.
    /// - **a limb narrower than the stroke** — five texels wide. Its blurred
    ///   coverage never reaches the value a straight edge reaches, so the
    ///   recovered distance is wrong everywhere near it and the stroke thins.
    /// - **holes smaller than the stroke** — a disc and a square. An outer
    ///   stroke should fill them solid; a blurred field says the middle of a
    ///   small hole is further from the edge than it is.
    ///
    /// Every axis-aligned edge sits on a texel boundary. That is what makes the
    /// flood's half-texel correction exact rather than approximate, and it is
    /// why the calibration column of the table comes out at the width asked
    /// for, on both methods, to a hundredth of a pixel.
    fn inside(x: f32, y: f32) -> bool {
        let (x, y) = (x - OFF, y - OFF);
        let body = (40.0..232.0).contains(&x) && (40.0..232.0).contains(&y);
        let hole_disc = (x - 100.0).powi(2) + (y - 100.0).powi(2) < 7.0 * 7.0;
        let hole_square = (150.0..164.0).contains(&x) && (92.0..106.0).contains(&y);
        let limb = (58.0..63.0).contains(&x) && (232.0..432.0).contains(&y);
        let spike = in_triangle(x, y, [232.0, 60.0], [232.0, 116.0], [480.0, 88.0]);
        let capsule = seg_distance(x, y, 270.0, 270.0, 430.0, 430.0) <= 22.0;
        (body && !hole_disc && !hole_square) || limb || spike || capsule
    }

    fn in_triangle(x: f32, y: f32, a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> bool {
        let side =
            |p: [f32; 2], q: [f32; 2]| (q[0] - p[0]) * (y - p[1]) - (q[1] - p[1]) * (x - p[0]);
        let (s1, s2, s3) = (side(a, b), side(b, c), side(c, a));
        (s1 >= 0.0 && s2 >= 0.0 && s3 >= 0.0) || (s1 <= 0.0 && s2 <= 0.0 && s3 <= 0.0)
    }

    fn seg_distance(x: f32, y: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
        let (vx, vy) = (bx - ax, by - ay);
        let (wx, wy) = (x - ax, y - ay);
        let t = ((wx * vx + wy * vy) / (vx * vx + vy * vy)).clamp(0.0, 1.0);
        ((wx - t * vx).powi(2) + (wy - t * vy).powi(2)).sqrt()
    }

    /// Where the calibration half-plane's edge sits.
    const FLAT_EDGE: f32 = SIZE as f32 / 2.0;

    /// A plain half-plane, and the only thing either method is *tuned* on.
    ///
    /// It has to be a separate picture rather than a straight edge of the shape
    /// itself, and the reason is the one thing a 64 px stroke makes visible: a
    /// blur of reach 129 reads a neighbourhood 259 texels across, and the
    /// shape's body is 192. Tuned against that edge the reach came out at 123
    /// where the arithmetic says 129 — six pixels of correction for the body's
    /// own far side being inside the kernel, quietly folded into every other
    /// column. Here the covered half runs the full height of the picture and
    /// clamp-to-edge extends it, so it is a half-plane exactly, and the tuning
    /// measures the kernel and nothing else.
    fn flat_bytes() -> Vec<u8> {
        coverage(|x, _| x < FLAT_EDGE)
    }

    /// The shape as RGBA the layer texture can take: alpha is the coverage,
    /// four by four supersampled, and the colour is nothing because an effect
    /// derives from alpha alone.
    ///
    /// The layer is `Rgba8UnormSrgb` and an sRGB format encodes RGB only, so
    /// the alpha byte written here is the linear coverage `fs_extract` reads.
    fn layer_bytes() -> Vec<u8> {
        coverage(inside)
    }

    fn coverage(shape: impl Fn(f32, f32) -> bool) -> Vec<u8> {
        let n = SIZE as usize;
        let mut out = vec![0u8; n * n * 4];
        for j in 0..n {
            for i in 0..n {
                let mut hits = 0u32;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let x = i as f32 + (sx as f32 + 0.5) / 4.0;
                        let y = j as f32 + (sy as f32 + 0.5) / 4.0;
                        if shape(x, y) {
                            hits += 1;
                        }
                    }
                }
                out[(j * n + i) * 4 + 3] = (hits * 255 / 16) as u8;
            }
        }
        out
    }

    // -----------------------------------------------------------------------
    // Targets
    // -----------------------------------------------------------------------

    struct Targets {
        layer: wgpu::TextureView,
        cov: wgpu::TextureView,
        wide_a: wgpu::TextureView,
        wide_b: wgpu::TextureView,
        small_a: wgpu::TextureView,
        small_b: wgpu::TextureView,
        byte_a: wgpu::TextureView,
        byte_b: wgpu::TextureView,
        seed_a: wgpu::TextureView,
        seed_b: wgpu::TextureView,
        out: wgpu::Texture,
        out_view: wgpu::TextureView,
        read: wgpu::Buffer,
    }

    impl Targets {
        fn new(gpu: &Gpu, pixels: &[u8]) -> Self {
            let small = SIZE / 4;
            let out = texture_of(gpu, "probe", SIZE, PROBE, wgpu::TextureUsages::COPY_SRC);
            let layer = texture_of(gpu, "layer", SIZE, RGBA, wgpu::TextureUsages::COPY_DST);
            gpu.queue.write_texture(
                layer.as_image_copy(),
                pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(SIZE * 4),
                    rows_per_image: Some(SIZE),
                },
                wgpu::Extent3d {
                    width: SIZE,
                    height: SIZE,
                    depth_or_array_layers: 1,
                },
            );
            Self {
                layer: layer.create_view(&Default::default()),
                cov: texture(gpu, "coverage", SIZE, R8),
                wide_a: texture(gpu, "wide-a", SIZE, WIDE),
                wide_b: texture(gpu, "wide-b", SIZE, WIDE),
                small_a: texture(gpu, "small-a", small, WIDE),
                small_b: texture(gpu, "small-b", small, WIDE),
                byte_a: texture(gpu, "byte-a", SIZE, R8),
                byte_b: texture(gpu, "byte-b", SIZE, R8),
                seed_a: texture(gpu, "seed-a", SIZE, SEED),
                seed_b: texture(gpu, "seed-b", SIZE, SEED),
                out_view: out.create_view(&Default::default()),
                out,
                read: gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("probe-read"),
                    size: (SIZE * SIZE * 4) as u64,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }),
            }
        }

        /// Run one bake into `out` and read it back. Red is the stroke's alpha
        /// after the knockout, green is the layer's coverage.
        ///
        /// One readback serves the picture and every measurement taken off it,
        /// so what is measured is the alpha the shader produced rather than a
        /// colour ramp made from it afterwards.
        fn run(
            &self,
            gpu: &Gpu,
            bench: &Bench,
            build: impl FnOnce(&mut wgpu::CommandEncoder),
        ) -> (Field, Field) {
            // 768 x 4 is 3072, a multiple of the 256-byte row alignment, so
            // there is no padding to unpick. Asserted rather than assumed: a
            // later size that is not would read back as diagonal stripes and
            // look like a shader bug.
            const _: () = assert!((SIZE * 4).is_multiple_of(256));

            let mut enc = gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
            record(
                gpu,
                bench,
                &mut enc,
                Pipe::Extract,
                &self.layer,
                &self.layer,
                &self.cov,
                SIZE,
                cfg_blur(SIZE, 0, 0),
            );
            build(&mut enc);
            enc.copy_texture_to_buffer(
                self.out.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &self.read,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(SIZE * 4),
                        rows_per_image: Some(SIZE),
                    },
                },
                wgpu::Extent3d {
                    width: SIZE,
                    height: SIZE,
                    depth_or_array_layers: 1,
                },
            );
            gpu.queue.submit([enc.finish()]);
            self.read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
            let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
            let (stroke, cov) = {
                let view = self.read.slice(..).get_mapped_range();
                let n = (SIZE * SIZE) as usize;
                let mut stroke = Vec::with_capacity(n);
                let mut cov = Vec::with_capacity(n);
                for p in view.chunks_exact(4).take(n) {
                    stroke.push(f32::from(p[0]) / 255.0);
                    cov.push(f32::from(p[1]) / 255.0);
                }
                (stroke, cov)
            };
            self.read.unmap();
            (Field { a: stroke }, Field { a: cov })
        }

        fn jfa(&self, gpu: &Gpu, bench: &Bench, width: u32) -> (Field, Field) {
            let steps = (32 - width.max(1).leading_zeros()).max(1);
            self.run(gpu, bench, |enc| {
                record(
                    gpu,
                    bench,
                    enc,
                    Pipe::Seed,
                    &self.seed_b,
                    &self.cov,
                    &self.seed_a,
                    SIZE,
                    cfg_blur(SIZE, 0, 0),
                );
                let mut from_a = true;
                for i in 0..steps {
                    let (src, dst) = if from_a {
                        (&self.seed_a, &self.seed_b)
                    } else {
                        (&self.seed_b, &self.seed_a)
                    };
                    let mut c = cfg_blur(SIZE, 0, 0);
                    c.k = 1i32 << (steps - 1 - i);
                    record(gpu, bench, enc, Pipe::Flood, src, &self.cov, dst, SIZE, c);
                    from_a = !from_a;
                }
                let last = if from_a { &self.seed_a } else { &self.seed_b };
                let mut c = cfg_blur(SIZE, 0, 0);
                c.width = width as f32;
                record(
                    gpu,
                    bench,
                    enc,
                    Pipe::StrokeProbe,
                    last,
                    &self.cov,
                    &self.out_view,
                    SIZE,
                    c,
                );
            })
        }

        /// Blur and threshold at full resolution, through either the wide
        /// intermediate or the eight-bit one.
        fn blur(
            &self,
            gpu: &Gpu,
            bench: &Bench,
            width: u32,
            wide: bool,
            reach: f32,
            r: i32,
        ) -> (Field, Field) {
            let (pipe, a, b) = if wide {
                (Pipe::BoxWide, &self.wide_a, &self.wide_b)
            } else {
                (Pipe::Box, &self.byte_a, &self.byte_b)
            };
            let mut resolve = cfg_blur(SIZE, 0, 0);
            resolve.width = width as f32;
            resolve.reach = reach;
            self.run(gpu, bench, |enc| {
                for (src, dst, axis) in [(&self.cov, a, 0), (a, b, 0), (b, a, 1), (a, b, 1)] {
                    record(
                        gpu,
                        bench,
                        enc,
                        pipe,
                        src,
                        src,
                        dst,
                        SIZE,
                        cfg_blur(SIZE, r, axis),
                    );
                }
                record(
                    gpu,
                    bench,
                    enc,
                    Pipe::BlurProbe,
                    b,
                    &self.cov,
                    &self.out_view,
                    SIZE,
                    resolve,
                );
            })
        }

        /// The version that really does reuse the shadow's own blur: the same
        /// tent, run on a quarter-resolution copy and bilinearly upsampled.
        fn blur_quarter(
            &self,
            gpu: &Gpu,
            bench: &Bench,
            width: u32,
            reach: f32,
            full_r: i32,
        ) -> (Field, Field) {
            let small = SIZE / 4;
            let r = full_r.div_euclid(4).max(1);
            let mut resolve = cfg_blur(SIZE, 0, 0);
            resolve.width = width as f32;
            resolve.reach = reach;
            self.run(gpu, bench, |enc| {
                record(
                    gpu,
                    bench,
                    enc,
                    Pipe::DownWide,
                    &self.cov,
                    &self.cov,
                    &self.small_a,
                    small,
                    cfg_blur(small, 0, 0),
                );
                for (src, dst, axis) in [
                    (&self.small_a, &self.small_b, 0),
                    (&self.small_b, &self.small_a, 0),
                    (&self.small_a, &self.small_b, 1),
                    (&self.small_b, &self.small_a, 1),
                ] {
                    record(
                        gpu,
                        bench,
                        enc,
                        Pipe::BoxWide,
                        src,
                        src,
                        dst,
                        small,
                        cfg_blur(small, r, axis),
                    );
                }
                record(
                    gpu,
                    bench,
                    enc,
                    Pipe::BlurProbeUp,
                    &self.small_a,
                    &self.cov,
                    &self.out_view,
                    SIZE,
                    resolve,
                );
            })
        }
    }

    // -----------------------------------------------------------------------
    // Measuring what the picture shows
    // -----------------------------------------------------------------------

    const STEP: f32 = 0.02;

    /// How far a stroke reaches along a ray — or why that cannot be said.
    ///
    /// **The two failures are opposites and reporting both as "none" is how a
    /// false headline gets written.** Under a loose kernel the thin limb's
    /// reading was "none" and the sentence attached to it was "no stroke around
    /// the limb"; the picture showed the limb *engulfed* in the body's own
    /// stroke, which is the other end of the same scale. They also point
    /// opposite ways for the calibration below, which is what sent it to a
    /// clamp and printed the rail as an answer.
    #[derive(Clone, Copy)]
    enum Reach {
        At(f32),
        /// Nothing on this ray is more than half covered.
        None_,
        /// The ray never leaves the stroke, so its far edge is past the window.
        Engulfed,
    }

    impl Reach {
        /// As a width the calibration can order. Zero and infinity, because
        /// that is what the two failures *mean* — a stroke narrower than
        /// anything and one wider than the window.
        fn width(self) -> f32 {
            match self {
                Self::At(t) => t,
                Self::None_ => 0.0,
                Self::Engulfed => f32::INFINITY,
            }
        }

        fn found(self) -> Option<f32> {
            match self {
                Self::At(t) => Some(t),
                _ => None,
            }
        }
    }

    /// Where the stroke's outer contour crosses a half, along a ray.
    ///
    /// The *outermost* crossing, not the first. An outer stroke is knocked out
    /// under the layer, so a ray starting at the shape's own edge begins in a
    /// texel that is half inside and reads below a half; taking the first
    /// crossing would measure the knockout instead of the stroke.
    fn contour(f: &Field, origin: (f32, f32), dir: (f32, f32), max_t: f32) -> Reach {
        let at = |t: f32| f.at(origin.0 - 0.5 + dir.0 * t, origin.1 - 0.5 + dir.1 * t);
        let mut answer = None;
        let mut peak: f32 = at(0.0);
        let mut prev = (0.0f32, peak);
        let mut t = STEP;
        while t <= max_t {
            let v = at(t);
            peak = peak.max(v);
            if prev.1 >= 0.5 && v < 0.5 {
                answer = Some(prev.0 + STEP * cross(prev.1, v));
            }
            prev = (t, v);
            t += STEP;
        }
        match answer {
            Some(t) => Reach::At(t),
            None if peak >= 0.5 => Reach::Engulfed,
            None => Reach::None_,
        }
    }

    /// Where between two samples the half lies, as a fraction of the step.
    fn cross(before: f32, after: f32) -> f32 {
        let span = before - after;
        if span.abs() > f32::EPSILON {
            (before - 0.5) / span
        } else {
            0.0
        }
    }

    /// Everything one bake is judged on, in pixels.
    struct Probe {
        axis: Reach,
        corner: Reach,
        apex: Reach,
        diagonal: Reach,
        limb_span: Reach,
        hole_fill: f32,
    }

    const ROOT_HALF: f32 = std::f32::consts::FRAC_1_SQRT_2;

    impl Probe {
        /// The worst any one feature is out by, in pixels, against what a true
        /// disc would draw.
        ///
        /// **The selector is stated because it decides the answer.** The
        /// obvious one — mean absolute alpha against the flood — is dominated
        /// by the area where both methods agree by construction, so it rewards
        /// a setting that is nearly right over most of the picture and badly
        /// wrong at four corners. This is the pessimistic reading instead, and
        /// the two do not always pick the same row.
        ///
        /// The apex is **excluded**, and not because it is inconvenient: the
        /// spike rasterises short of its own point, so there is no ideal to
        /// measure against. Including it would drive the choice to the floor of
        /// the kernel range, where the apex is best and the corner bulges by a
        /// quarter of the stroke's width. The axis is excluded because it is
        /// what was tuned.
        fn worst_error(&self, width: u32) -> f32 {
            let w = width as f32;
            let off = |r: Reach, ideal: f32| match r {
                Reach::At(t) => (t - ideal).abs(),
                _ => f32::INFINITY,
            };
            off(self.corner, w)
                .max(off(self.diagonal, w))
                .max(off(self.limb_span, 5.0 + 2.0 * w))
        }
    }

    /// How far out to march. Two widths plus a margin: the contour sits at
    /// about one width and nothing here is interested in a stroke that
    /// over-reaches by more than double.
    fn ray_reach(width: u32) -> f32 {
        width as f32 * 2.0 + 12.0
    }

    /// The half-plane's edge, on the calibration picture. This is what a reach
    /// is tuned against, and both methods have to answer the width they were
    /// asked for here or nothing else in the table is a comparison.
    fn flat_contour(f: &Field) -> Reach {
        // The covered half is the *left* one, so the outward ray runs right.
        //
        // A wider window than the probes use, because the search below tries
        // reaches that put the contour a long way out and has to be able to
        // see how far rather than only that it missed.
        contour(f, (FLAT_EDGE, FLAT_EDGE + 0.5), (1.0, 0.0), FLAT_EDGE - 8.0)
    }

    /// The same reading on the shape's own left edge. Not the calibration —
    /// what it shows is how far a finite body moves an answer the half-plane
    /// gets exactly right, which at 64 px is most of a pixel.
    fn axis_contour(f: &Field, width: u32) -> Reach {
        contour(f, (OFF + 40.0, OFF + 160.5), (-1.0, 0.0), ray_reach(width))
    }

    fn probe(f: &Field, width: u32) -> Probe {
        let reach = ray_reach(width);
        Probe {
            axis: axis_contour(f, width),
            corner: contour(f, (OFF + 40.0, OFF + 40.0), (-ROOT_HALF, -ROOT_HALF), reach),
            // From the *nominal* apex. The spike narrows to nothing, so its
            // last few texels rasterise below half coverage and the shape
            // Umber actually seeds stops short of this point — which is why
            // the table prints no ideal for this column and the jump flood's
            // own answer is the reference. What the column is for is the
            // categorical reading: whether a stroke appears there at all.
            apex: contour(f, (OFF + 480.0, OFF + 88.0), (1.0, 0.0), reach),
            // On the capsule's lower-left flank, at its midpoint, along the
            // normal — y runs down, so the offset that looks like "up" is not.
            //
            // Like the corner's ray, this one carries about a fifth of a pixel
            // of the probe's own residual: the flood's nearest seed on a
            // diagonal is `t + 0.707` away rather than `t + 0.5`, so the half
            // texel `fs_stroke_probe` takes off leaves 0.21 short. It is why
            // the flood reads 19.78 and 63.79 against an ideal of 20 and 64,
            // and it is the probe rather than the flood.
            diagonal: contour(
                f,
                (
                    OFF + 350.0 - 22.0 * ROOT_HALF,
                    OFF + 350.0 + 22.0 * ROOT_HALF,
                ),
                (-ROOT_HALF, ROOT_HALF),
                reach,
            ),
            limb_span: limb_span(f, width),
            hole_fill: hole_fill(f),
        }
    }

    /// How wide the stroke around the five-texel limb comes out, outer edge to
    /// outer edge. A true disc answers `5 + 2w`.
    fn limb_span(f: &Field, width: u32) -> Reach {
        let y = OFF + 350.0 - 0.5;
        let (mut left, mut right) = (None, None);
        let start = OFF + 58.0 - width as f32 - 10.0;
        let end = OFF + 63.0 + width as f32 + 10.0;
        let mut x = start;
        let mut prev = f.at(x - 0.5, y);
        let mut peak: f32 = prev;
        let mut floor: f32 = prev;
        while x <= end {
            let v = f.at(x - 0.5, y);
            peak = peak.max(v);
            floor = floor.min(v);
            if prev < 0.5 && v >= 0.5 && left.is_none() {
                left = Some(x - STEP * cross(v, prev));
            }
            if prev >= 0.5 && v < 0.5 {
                right = Some(x - STEP + STEP * cross(prev, v));
            }
            prev = v;
            x += STEP;
        }
        match (left, right) {
            (Some(l), Some(r)) if r > l => Reach::At(r - l),
            // The window is entirely inside the stroke: the limb is swallowed
            // by something larger, not bare. Under a loose kernel this was the
            // *usual* answer at 64 px and it read as the opposite.
            _ if floor >= 0.5 => Reach::Engulfed,
            _ if peak < 0.5 => Reach::None_,
            // One edge only, which is the engulfed case seen from a window
            // that happens to clear the stroke on one side.
            _ => Reach::Engulfed,
        }
    }

    /// The disc hole's centre and radius, in continuous coordinates.
    const HOLE: (f32, f32, f32) = (OFF + 100.0, OFF + 100.0, 7.0);

    /// How far a texel centre is from the hole's centre, or `None` if it is not
    /// well inside it. One texel of slack, so the hole's own antialiased rim is
    /// not what is being averaged.
    fn hole_offset(i: i32, j: i32) -> Option<f32> {
        let rho = ((i as f32 + 0.5 - HOLE.0).powi(2) + (j as f32 + 0.5 - HOLE.1).powi(2)).sqrt();
        (rho < HOLE.2 - 1.0).then_some(rho)
    }

    fn hole_texels() -> impl Iterator<Item = (i32, i32, f32)> {
        let lo = (HOLE.0 - HOLE.2 - 1.0) as i32;
        let hi = (HOLE.0 + HOLE.2 + 1.0) as i32;
        (lo..=hi).flat_map(move |j| {
            (lo..=hi).filter_map(move |i| hole_offset(i, j).map(|rho| (i, j, rho)))
        })
    }

    /// The mean stroke alpha inside the disc hole. Every point in it is at most
    /// its own radius from the edge, so a stroke that wide or wider should fill
    /// it solid.
    fn hole_fill(f: &Field) -> f32 {
        let (mut sum, mut n) = (0.0, 0.0);
        for (i, j, _) in hole_texels() {
            sum += f.get(i, j);
            n += 1.0;
        }
        if n > 0.0 { sum / n } else { 0.0 }
    }

    /// What a true disc would answer over the same texels, so the hole's ideal
    /// is not a number somebody worked out by hand.
    fn ideal_hole_fill(width: u32) -> f32 {
        let (mut sum, mut n) = (0.0, 0.0);
        for (_, _, rho) in hole_texels() {
            sum += if HOLE.2 - rho <= width as f32 {
                1.0
            } else {
                0.0
            };
            n += 1.0;
        }
        if n > 0.0 { sum / n } else { 0.0 }
    }

    // -----------------------------------------------------------------------
    // Drawing it out
    // -----------------------------------------------------------------------

    const BG: [f32; 3] = [247.0, 245.0, 242.0];
    const INK: [f32; 3] = [56.0, 62.0, 70.0];
    const MARK: [f32; 3] = [214.0, 110.0, 32.0];
    const HOT: [f32; 3] = [200.0, 24.0, 24.0];

    fn compose(stroke: &Field, cov: &Field) -> Vec<u8> {
        let n = (SIZE * SIZE) as usize;
        let mut out = Vec::with_capacity(n * 3);
        for p in 0..n {
            let (c, s) = (cov.a[p], stroke.a[p]);
            for k in 0..3 {
                let base = BG[k] * (1.0 - c) + INK[k] * c;
                out.push((base * (1.0 - s) + MARK[k] * s).round().clamp(0.0, 255.0) as u8);
            }
        }
        out
    }

    /// White where the two agree, red where they do not, linear and with no
    /// amplification — a difference picture that exaggerates is one that has
    /// decided the answer before anybody looks at it.
    fn difference(a: &Field, b: &Field) -> (Vec<u8>, f32, f32) {
        let n = (SIZE * SIZE) as usize;
        let mut out = Vec::with_capacity(n * 3);
        let (mut worst, mut sum) = (0.0f32, 0.0f64);
        for p in 0..n {
            let d = (a.a[p] - b.a[p]).abs();
            worst = worst.max(d);
            sum += f64::from(d);
            for k in 0..3 {
                out.push((BG[k] * (1.0 - d) + HOT[k] * d).round().clamp(0.0, 255.0) as u8);
            }
        }
        (out, worst, (sum / n as f64) as f32)
    }

    fn write_png(dir: &std::path::Path, name: &str, w: u32, h: u32, rgb: &[u8]) {
        let path = dir.join(name);
        let file = match std::fs::File::create(&path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("  !! {}: {e}", path.display());
                return;
            }
        };
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        if let Err(e) = enc
            .write_header()
            .and_then(|mut wr| wr.write_image_data(rgb))
        {
            eprintln!("  !! {}: {e}", path.display());
        }
    }

    /// Four panels in a two by two grid, so one file answers "what is the
    /// difference" without a viewer that can flip between tabs.
    fn sheet(panels: [&[u8]; 4]) -> (Vec<u8>, u32) {
        let gap = 8u32;
        let side = SIZE * 2 + gap * 3;
        let mut out = vec![24u8; (side * side * 3) as usize];
        for (n, panel) in panels.iter().enumerate() {
            let ox = gap + (n as u32 % 2) * (SIZE + gap);
            let oy = gap + (n as u32 / 2) * (SIZE + gap);
            for j in 0..SIZE {
                let dst = (((oy + j) * side + ox) * 3) as usize;
                let src = (j * SIZE * 3) as usize;
                out[dst..dst + (SIZE * 3) as usize]
                    .copy_from_slice(&panel[src..src + (SIZE * 3) as usize]);
            }
        }
        (out, side)
    }

    /// "none" and "engulfed" are opposite answers and must not print alike.
    fn px(v: Reach) -> String {
        match v {
            Reach::At(t) => format!("{t:.2}"),
            Reach::None_ => "none".into(),
            Reach::Engulfed => "engulf".into(),
        }
    }

    /// Tune a blur path's reach until a straight axis-aligned edge comes out at
    /// exactly the width asked for.
    ///
    /// **Without this the comparison would be void.** The recovered distance is
    /// linear in the reach, so the reach is the one number that decides how
    /// wide the stroke comes out on a straight edge — and a comparison of two
    /// strokes that are not the same width is a comparison of nothing. The
    /// analytic answer for the full-resolution path is `2r + 1`, the continuous
    /// half support of two discrete boxes of `2r + 1` taps convolved, and the
    /// calibration lands within a hundredth of it, which is the check that says
    /// the arithmetic is right. The quarter-resolution path has no closed form
    /// worth writing: its kernel is that tent convolved with the downsample's
    /// own box and the bilinear upsample, which is not a tent, so its reach is
    /// found rather than derived.
    ///
    /// Nothing tunes the jump flood. Its distance is exact by construction and
    /// the calibration column proves it: it reads the width asked for without
    /// any parameter having been moved.
    /// A tuned reach and the width it actually produced.
    ///
    /// The second half is not diagnostics for its own sake. A path whose
    /// calibration does not converge is one that cannot draw the width it was
    /// asked for *at all*, and every figure measured off it afterwards is
    /// meaningless — so the caller has to be able to say so rather than print
    /// numbers. The quarter-resolution path fails here at ordinary widths, and
    /// finding that out took an hour of reading a table that looked merely
    /// poor.
    struct Tuned {
        reach: f32,
        got: Reach,
    }

    impl Tuned {
        fn converged(&self, target: u32) -> bool {
            self.got
                .found()
                .is_some_and(|g| (g - target as f32).abs() < 0.05)
        }
    }

    /// Bracket, then bisect, on the width the picture actually shows.
    ///
    /// It was a secant on `axis(x).unwrap_or(0.0)`, and that was wrong in a way
    /// that produced plausible numbers rather than an error. A reach too small
    /// makes the recovered distance never reach the stroke's own width, so the
    /// whole of the kernel's support comes out solid and the ray finds no
    /// crossing — a `None` meaning *infinitely wide*, scored there as **zero**,
    /// which is the exact opposite. The search then ran away, was clamped to
    /// `guess * 5`, and printed the rail as a result: the quarter-resolution
    /// path's "reach 120.0" was that, and the table under it was a measurement
    /// of nothing.
    ///
    /// The width is monotone decreasing in the reach — a longer reach maps the
    /// same blurred value to a larger distance, so the contour comes in — so a
    /// bracket and a bisection need no derivative and cannot run away. `Reach`
    /// is what orders the two failures for it: nothing is zero, engulfed is
    /// infinity.
    fn calibrate(target: u32, guess: f32, mut axis: impl FnMut(f32) -> Reach) -> Tuned {
        let want = target as f32;

        // Widen until the answer is bracketed: too wide at `lo`, too narrow at
        // `hi`. Sixteen steps at these ratios covers a factor of several
        // hundred either way, which is far more than any kernel here is out by.
        let mut lo = guess;
        let mut lo_w = axis(lo).width();
        for _ in 0..16 {
            if lo_w >= want {
                break;
            }
            lo *= 0.75;
            lo_w = axis(lo).width();
        }
        let mut hi = lo.max(guess);
        let mut hi_w = axis(hi).width();
        for _ in 0..16 {
            if hi_w <= want {
                break;
            }
            hi *= 1.35;
            hi_w = axis(hi).width();
        }
        if lo_w < want || hi_w > want {
            return Tuned {
                reach: guess,
                got: axis(guess),
            };
        }

        for _ in 0..24 {
            let mid = 0.5 * (lo + hi);
            if axis(mid).width() > want {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let reach = 0.5 * (lo + hi);
        Tuned {
            reach,
            got: axis(reach),
        }
    }

    fn row(w: &str, m: &str, p: &Probe, worst: Option<f32>, mean: Option<f32>) {
        println!(
            "  {:<7} {:<13} {:>6} {:>7} {:>6} {:>9} {:>10} {:>6} {:>7} {:>7}",
            w,
            m,
            px(p.axis),
            px(p.corner),
            px(p.apex),
            px(p.diagonal),
            px(p.limb_span),
            format!("{:.2}", p.hole_fill),
            worst.map_or_else(|| "-".into(), |v| format!("{v:.3}")),
            mean.map_or_else(|| "-".into(), |v| format!("{v:.4}")),
        );
    }

    /// Kernel widths swept, as a multiple of the stroke's own width.
    ///
    /// 0.5 is the floor `blur_radius_for` enforces and 2.0 is a generously
    /// smooth setting; the bottom of the range is sampled finely because that
    /// is where the answer moves fastest.
    const FACTORS: [f32; 8] = [0.5, 0.55, 0.6, 0.7, 0.85, 1.0, 1.5, 2.0];

    /// What blur-and-threshold does across the one setting that decides it.
    ///
    /// Reporting the method at a single kernel width would be a straw man, and
    /// the direction of the trade is not the one anybody guesses: the tighter
    /// the kernel, the *better* the corner and the thin limb, because the
    /// stroke's contour then sits deep in the kernel's tail where a corner's
    /// blurred value and an edge's have not yet diverged. It is also cheaper
    /// there. What bounds it is the floor at `w/2`, below which the recovered
    /// distance cannot reach the contour at all.
    fn radius_sweep(gpu: &Gpu, bench: &Bench, t: &Targets, flat: &Targets, w: u32) -> Option<i32> {
        println!("  {w} px, blur 16-bit at full resolution, across kernel widths:");
        println!(
            "    {:>6} {:>7} {:>6} {:>7} {:>6} {:>9} {:>10} {:>6} {:>7} {:>7}",
            "h/w",
            "box r",
            "axis",
            "corner",
            "apex",
            "diagonal",
            "limb span",
            "hole",
            "worst",
            "mean d",
        );
        let mut best: Option<(f32, i32)> = None;
        let (jfa, _) = t.jfa(gpu, bench, w);
        // A radius is a whole number of texels, so at a small width several
        // factors land on the same kernel. Printing the row four times would
        // read as four measurements agreeing.
        let mut seen = None;
        for factor in FACTORS {
            let r = blur_radius_for(w, factor);
            if seen == Some(r) {
                continue;
            }
            seen = Some(r);
            let reach = calibrate(w, 2.0 * r as f32 + 1.0, |k| {
                flat_contour(&flat.blur(gpu, bench, w, true, k, r).0)
            });
            let (f, _) = t.blur(gpu, bench, w, true, reach.reach, r);
            let p = probe(&f, w);
            let (_, _, mean) = difference(&jfa, &f);
            if !reach.converged(w) {
                println!(
                    "    {:>6} {:>7}   no reach draws {w} px on a half-plane; nearest {}",
                    format!("{:.2}", (2 * r + 1) as f32 / w as f32),
                    r,
                    px(reach.got),
                );
                continue;
            }
            let worst = p.worst_error(w);
            if best.is_none_or(|(b, _)| worst < b) {
                best = Some((worst, r));
            }
            println!(
                "    {:>6} {:>7} {:>6} {:>7} {:>6} {:>9} {:>10} {:>6} {:>7} {:>7}",
                format!("{:.2}", (2 * r + 1) as f32 / w as f32),
                r,
                px(p.axis),
                px(p.corner),
                px(p.apex),
                px(p.diagonal),
                px(p.limb_span),
                format!("{:.2}", p.hole_fill),
                if worst.is_finite() {
                    format!("{worst:.2}")
                } else {
                    "-".into()
                },
                format!("{mean:.4}"),
            );
        }
        best.map(|(_, r)| r)
    }

    pub(super) fn draw(gpu: &Gpu, bench: &Bench, dir: &std::path::Path, ok: bool) {
        if !ok {
            println!("pictures: skipped, this device cannot render one of the two paths\n");
            return;
        }
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("pictures: {}: {e}", dir.display());
            return;
        }
        println!("pictures: {}", dir.display());

        let t = Targets::new(gpu, &layer_bytes());
        let flat = Targets::new(gpu, &flat_bytes());

        // The shape on its own, so the features being talked about can be
        // pointed at.
        let (_, cov) = t.jfa(gpu, bench, 1);
        let blank = Field {
            a: vec![0.0; cov.a.len()],
        };
        write_png(dir, "shape.png", SIZE, SIZE, &compose(&blank, &cov));

        println!();
        println!("  pixels; every method was tuned to draw the first column's width on a");
        println!("  half-plane, and 'axis' is the same reading on the shape's own left edge");
        println!(
            "  {:<7} {:<13} {:>6} {:>7} {:>6} {:>9} {:>10} {:>6} {:>7} {:>7}",
            "width",
            "method",
            "axis",
            "corner",
            "apex",
            "diagonal",
            "limb span",
            "hole",
            "max d",
            "mean d",
        );

        for w in WIDTHS {
            // The headline row is the sweep's own best, not `BLUR_FACTOR`.
            // There is no single factor that is best at every width — the
            // optimum drifts from about 1.0 at 2 px to 0.55 at 64 — so pinning
            // one and calling the result "the blur" would be reporting the
            // method at a setting nobody would choose for that stroke. The
            // *timed* columns still use `BLUR_FACTOR`, which is the cheapest of
            // the useful settings and therefore generous to the method on the
            // one axis a recommendation against it would be accused of rigging.
            let r = radius_sweep(gpu, bench, &t, &flat, w)
                .unwrap_or_else(|| blur_radius_for(w, BLUR_FACTOR));
            let full_guess = 2.0 * r as f32 + 1.0;
            let small_guess = 8.0 * r.div_euclid(4).max(1) as f32 + 8.0;
            let wide_reach = calibrate(w, full_guess, |k| {
                flat_contour(&flat.blur(gpu, bench, w, true, k, r).0)
            });
            let byte_reach = calibrate(w, full_guess, |k| {
                flat_contour(&flat.blur(gpu, bench, w, false, k, r).0)
            });
            let quarter_reach = calibrate(w, small_guess, |k| {
                flat_contour(&flat.blur_quarter(gpu, bench, w, k, r).0)
            });
            let flood_flat = flat_contour(&flat.jfa(gpu, bench, w).0);

            let (jfa, cov) = t.jfa(gpu, bench, w);
            let (blur, _) = t.blur(gpu, bench, w, true, wide_reach.reach, r);
            let (blur8, _) = t.blur(gpu, bench, w, false, byte_reach.reach, r);
            let (quarter, _) = t.blur_quarter(gpu, bench, w, quarter_reach.reach, r);

            let jfa_rgb = compose(&jfa, &cov);
            let blur_rgb = compose(&blur, &cov);
            let blur8_rgb = compose(&blur8, &cov);
            let quarter_rgb = compose(&quarter, &cov);
            let (diff_rgb, d_max, d_mean) = difference(&jfa, &blur);
            let (diffq_rgb, q_max, q_mean) = difference(&jfa, &quarter);
            let (_, e_max, e_mean) = difference(&blur, &blur8);

            // **The kernel goes in the filename.** `dist/` is ignored, so
            // nothing in git ties a picture to the run that made it, and a
            // reviewer comparing a stale `w20-blur.png` against a fresh table
            // reads the mismatch as a bug in the probes rather than as a stale
            // file — which happened, twice, and cost an hour. With `r` in the
            // name a stale picture sits visibly beside the current one instead
            // of quietly replacing it.
            let named = |what: &str| format!("w{w:02}r{r:02}-{what}.png");
            write_png(dir, &named("jfa"), SIZE, SIZE, &jfa_rgb);
            write_png(dir, &named("blur"), SIZE, SIZE, &blur_rgb);
            write_png(dir, &named("blur-8bit"), SIZE, SIZE, &blur8_rgb);
            write_png(dir, &named("blur-quarter"), SIZE, SIZE, &quarter_rgb);
            write_png(dir, &named("diff-blur"), SIZE, SIZE, &diff_rgb);
            write_png(dir, &named("diff-blur-quarter"), SIZE, SIZE, &diffq_rgb);
            let (grid, side) = sheet([&jfa_rgb, &blur_rgb, &quarter_rgb, &diff_rgb]);
            write_png(dir, &named("sheet"), side, side, &grid);

            row(
                &format!("{w} px"),
                "jump flood",
                &probe(&jfa, w),
                None,
                None,
            );
            row(
                "",
                "blur 16-bit",
                &probe(&blur, w),
                Some(d_max),
                Some(d_mean),
            );
            row(
                "",
                "blur 8-bit",
                &probe(&blur8, w),
                Some(e_max),
                Some(e_mean),
            );
            if quarter_reach.converged(w) {
                row(
                    "",
                    "blur quarter",
                    &probe(&quarter, w),
                    Some(q_max),
                    Some(q_mean),
                );
            } else {
                // Not "poor" — *unusable*. No reach makes this path draw the
                // width it was asked for on a straight edge, so every figure
                // taken off it would be a measurement of a stroke that is not
                // the one being compared. It is the path §13 proposes, on the
                // grounds that the shadow's blur is being built anyway.
                println!(
                    "          blur quarter: no reach draws {w} px on a half-plane; \
                     nearest {}",
                    px(quarter_reach.got),
                );
            }
            println!(
                "  {:<7} {:<13} {:>6} {:>7} {:>6} {:>9} {:>10} {:>6}",
                "",
                "ideal",
                format!("{w:.2}"),
                format!("{w:.2}"),
                "-",
                format!("{w:.2}"),
                format!("{:.2}", 5.0 + 2.0 * w as f32),
                format!("{:.2}", ideal_hole_fill(w)),
            );
            println!(
                "          box r {r} (the sweep's best; timed at {BLUR_FACTOR} x width); \
                 half-plane: flood {}, reach tuned to full {:.1} \
                 (analytic {full_guess:.1}), 8-bit {:.1}, quarter {:.1}",
                px(flood_flat),
                wide_reach.reach,
                byte_reach.reach,
                quarter_reach.reach,
            );
            println!();
        }

        println!("  a sheet reads: jump flood, blur 16-bit / blur quarter, difference.");
        println!("  'max d' and 'mean d' are against the jump flood, except the 8-bit row,");
        println!("  which is against the 16-bit blur so the quantisation stands on its own.");
        println!("  the apex column has no ideal: the spike rasterises short of its own point,");
        println!("  so the jump flood's answer is the reference for that column.");
        println!("  'none' is no stroke on that ray; 'engulf' is the opposite, a ray that");
        println!("  never leaves one -- swallowed by a neighbouring feature's stroke.");
        println!("  the corner and diagonal rays carry about 0.21 px of the probe's own");
        println!("  residual, which is why the flood reads 19.78 and 20.41 against 20.");
        println!();
    }
}
