//! The canvas renderer: layer storage, stroke scratch surface and the three
//! pipelines that move data between them.

use bytemuck::{Pod, Zeroable};
use glam::{UVec2, Vec2};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use umber_core::{
    Affine, Anchor, Background, BlendMode, BrushMode, Camera, CanvasCopy, Color, Dab, FlipAxis,
    PixelRect, Selection, TipMask, transform,
};
use wgpu::util::DeviceExt;

/// Layer storage format.
///
/// `Srgb`, despite the engine working in linear throughout, because eight bits
/// of *linear* storage spends nearly all its precision on highlights: a dark
/// ink at linear 0.0056 lands on 1–2 of 255, so dark tones band badly and drift
/// a couple of sRGB levels between the float preview and the stored result. An
/// sRGB-typed target distributes precision perceptually. Blending stays correct
/// — the hardware decodes to linear, blends, and re-encodes on write.
const LAYER_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// What one pixel of [`LAYER_FORMAT`] costs. Four, and it is named because
/// [`grown_capacity`] reasons in bytes and a bare `* 4` beside a canvas size
/// reads as arbitrary.
const LAYER_BYTES_PER_PIXEL: u64 = 4;

/// The same bits, viewed without the transfer function.
///
/// Used by one pass and one pass only: [`CanvasRenderer::flip_layers`], which
/// has to be an exact permutation of texels. Reading through an sRGB view
/// decodes to linear and writing through one re-encodes, and a round trip
/// through that pair is a promise about rounding rather than about pixels —
/// which matters here more than anywhere else in the renderer, because undoing
/// a flip *is* another flip, so any drift compounds every time. Read as raw
/// `u8 / 255` and written back, an f32 carries the byte exactly.
///
/// Listed in the layer array's `view_formats`, which is what makes such a view
/// legal at all.
const LAYER_FORMAT_LINEAR: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// The stroke scratch only needs coverage, so one channel instead of four —
/// a 4x saving on the bandwidth of the hottest texture in the frame.
///
/// Eight bits because that is exactly the width of where the coverage is going:
/// `LAYER_FORMAT`'s alpha channel is linear 8-bit (an sRGB format encodes RGB
/// only), so commit re-quantises to 256 levels whatever the scratch held. The
/// scratch therefore adds no loss of its own, and widening it cannot make the
/// pen's 1024 pressure levels reach the canvas — only a wider *layer* could.
/// `a_pressure_step_finer_than_the_layer_makes_no_mark` pins that, and the
/// build-up target below has the one case where the width is not a wash.
///
/// Shared with the tip and grain masks, which are 8-bit source data anyway.
const STROKE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;

/// Instance buffer capacity, in dabs. A single frame only ever holds the dabs
/// generated since the last frame; 64k is far more than a 120 Hz frame of
/// even the fastest flick can produce.
const MAX_DABS_PER_FRAME: usize = 65_536;

/// Stack entries a document may hold. Mirrored by `LayerStack::MAX`.
///
/// It no longer sizes anything in `composite.wgsl` — see [`MAX_DRAWS`], which
/// does. What it bounds is the *stack*: layers and folders, the thing the
/// layers panel lists.
const MAX_LAYERS: usize = 64;

/// How deep the layer texture array may grow — **and the number every other
/// capacity in this block is derived from, because it is the only one that is
/// not ours to pick.**
///
/// A slice is a layer, a layer's mask, the spare a floating transform previews
/// into, or one baked effect. Mirrored by `LayerStack::MAX_SLOTS`.
///
/// `Gpu::new` requests `Limits::downlevel_defaults().using_resolution(…)`.
/// `downlevel_defaults` names the three texture *dimensions*, four buffer and
/// shader limits, and then falls through to `Limits::defaults()` — so
/// `max_texture_array_layers` is **256**, and `using_resolution` copies only
/// those three dimensions and cannot raise it. A 257th slice is a
/// `create_texture` validation error, and a validation error reaches
/// `crash::device_error`, which is fatal with a painting on screen.
///
/// `docs/layer-effects.md` §6.3 derived 257 — 64 layers, 64 masks, one float
/// spare and 128 effects — without checking that. It is one over, which is
/// exactly why nobody had looked: the previous ceiling of 129 sat 127 clear.
/// The assertion below is therefore the load-bearing part of this block, not
/// the comment.
const MAX_SLOTS: usize = 256;

/// **The ceiling is the device's, so it is read from the device's own limits
/// rather than restated.**
///
/// `Limits::downlevel_defaults` is a `const fn`, which is what lets this be a
/// compile error instead of a test. It has to be one: what it catches is not a
/// red test run but `create_texture` failing validation inside
/// [`CanvasRenderer::ensure_slots`], which goes through `on_uncaptured_error`
/// and takes the process down.
const _: () = assert!(
    MAX_SLOTS <= wgpu::Limits::downlevel_defaults().max_texture_array_layers as usize,
    "the layer array would be deeper than downlevel_defaults guarantees"
);

/// A full stack of masked layers and the float's spare must fit under the
/// ceiling, or [`MAX_EFFECT_SLICES`] underflows.
///
/// The underflow would already be a compile error, since these are consts, and
/// this does not claim to be reported first — both items are evaluated and
/// rustc chooses. What it adds is a *message*, where an arithmetic-overflow
/// diagnostic against an expression says nothing about which of two capacities
/// was set wrong.
///
/// Written `* 2 <` rather than the `* 2 + 1 <=` the sentence above describes,
/// because clippy's `int_plus_one` rejects the second spelling. They are the
/// same predicate on integers; the `+ 1` is the float's spare.
const _: () = assert!(
    MAX_LAYERS * 2 < MAX_SLOTS,
    "no room under the ceiling for a fully masked stack and a float"
);

/// What is left of [`MAX_SLOTS`] once every layer, every mask and the float's
/// spare have their slice: **127**.
///
/// Derived rather than written down, so a change to [`MAX_LAYERS`] carries
/// through instead of leaving numbers to be edited by hand. The float's spare
/// is inside the subtraction and never gives way — a transform must always have
/// somewhere to preview.
///
/// **Raising [`MAX_LAYERS`] *lowers* this, and lowers [`MAX_DRAWS`] with it.**
/// The ceiling is fixed by the device, so every layer added takes two slices out
/// of the effect budget. This is `MAX_SLOTS - (MAX_LAYERS * 2 + 1)` — 55 at a
/// stack cap of 100 and **1** at 127 — while `MAX_DRAWS` is
/// `MAX_SLOTS - MAX_LAYERS - 1`, which is 155 and 128 at the same two. They are
/// different quantities and this comment ran them together, quoting the effect
/// budget's 1 as though it were the draw cap; at 127 layers `MAX_DRAWS >=
/// MAX_LAYERS` is true because it is 128, not because 1 would have satisfied it.
/// The test loop computes both.
///
/// **Something does fail when it happens**, and this comment claimed otherwise:
/// `effect::BUDGET_DERIVATION` is a `const` assertion over `LayerStack::MAX`, so
/// raising the stack cap without re-deriving the model's own cap is a compile
/// error naming the reason. That was true when this was written — the model
/// branch had not landed — and false from the merge onwards. Anyone raising the
/// stack cap still has to decide whether the effect budget left is one worth
/// having; they will simply be told rather than left to find out.
///
/// It also does not carry through to the shader, which holds `MAX_DRAWS` as a
/// literal `191u`. That is a fourth number and it is changed by hand;
/// `the_three_draw_capacities_agree` is what makes forgetting it a red test
/// rather than a silent uniform mismatch.
///
/// **127 rather than 128 is also what makes the cap reachable**, which
/// `docs/layer-effects.md` §6.3 records and is worth repeating where the number
/// is: with two effect kinds and one of each per layer, 64 layers can enable at
/// most 128, so against a budget of 128 the refusal sits exactly on the ceiling
/// and can only be exercised by a stack the model forbids. Against 127 the last
/// effect on a fully doubled stack is refused for real. **Re-check that when an
/// effect kind is added** — the arithmetic moves and nothing here will say so.
const MAX_EFFECT_SLICES: usize = effect_slices(MAX_LAYERS, MAX_SLOTS);

/// Entries the composite pass's two uniform arrays carry, mirrored by
/// `MAX_DRAWS` in `composite.wgsl`: **191**.
///
/// **A draw is not a stack entry**, which is why this is not [`MAX_LAYERS`].
/// One layer composites as one draw today; a layer carrying effects composites
/// as several, each with its own slot, opacity and blend mode, because a shadow
/// at Multiply has to multiply against what is *under* the layer.
/// `docs/layer-effects.md` §6.2 has the argument.
///
/// **It is `MAX_LAYERS + MAX_EFFECT_SLICES` and not a round number**, because
/// an effect draw reads an effect *slice* — one draw, one slice — so the draw
/// budget cannot exceed the slice budget. §6.2 says 192, which was derived from
/// the 257 the ceiling refuses; a 192nd entry would be a draw with nowhere to
/// read from.
///
/// The cost of raising it is uniform bytes and the upload, and nothing per
/// fragment: the loop in `composite.wgsl` is bounded by `layer_count`. The
/// bytes are counted in [`ViewUniforms`].
const MAX_DRAWS: usize = MAX_LAYERS + MAX_EFFECT_SLICES;

/// Slices left for effects once a stack of `layers`, all masked, and the
/// float's spare have theirs, under a ceiling of `ceiling`.
///
/// **A `const fn` rather than an expression inline in [`MAX_EFFECT_SLICES`],
/// and that is what makes the derivation testable at all.** A test that
/// recomputes `ceiling - (layers * 2 + 1)` for itself and then asserts
/// `layers * 2 + 1 + that == ceiling` has written `a - b + b == a`, which holds
/// for every function body there is — that was the second draft of
/// `the_slice_ceiling_agrees_with_umber_core` and it tested nothing. Calling
/// *this* turns the same assertion into a statement about the rule: a body
/// correct at 64 and wrong elsewhere fails it, which was checked by writing
/// one.
const fn effect_slices(layers: usize, ceiling: usize) -> usize {
    ceiling - (layers * 2 + 1)
}

/// Texture-array slices allocated up front. Growth doubles this while the array
/// is cheap, so a typical document never pays for a copy — see
/// [`grown_capacity`].
const INITIAL_SLOTS: u32 = 4;

/// How large the layer array may grow **by doubling**.
///
/// Doubling is the right way to grow a collection whose elements are cheap and
/// the wrong way to grow one whose elements are canvas-sized. A slice is
/// `width × height × 4`, so the same "one more slice" costs 256 KiB at 256² and
/// 400 MB at 10000², and the overshoot is what a budget counted in *slices*
/// cannot see. Stating it in bytes is what makes the policy canvas-aware.
///
/// A quarter of a gigabyte of speculative texture is the trade: enough that an
/// ordinary document never reallocates twice for the same layer, small enough
/// that the waste can never dominate a working set. At 2048² it allows doubling
/// to 16 slices; at 10000² it allows none at all, which is correct — nothing
/// should speculatively allocate 400 MB.
const GROWTH_DOUBLING_BUDGET_BYTES: u64 = 256 << 20;

/// The capacity to allocate so that `needed` slices exist, given how large one
/// slice is.
///
/// **Double while the resulting array would stay inside
/// [`GROWTH_DOUBLING_BUDGET_BYTES`]; past that, grow to exactly `needed`.**
///
/// A pure function so the policy is testable without a device, which is the
/// arrangement [`band_rows`] already keeps and the only one that works here: the
/// case that matters is a 129th slice at 2048², and allocating it for real is
/// two gigabytes of texture nobody can ask a CI runner for.
///
/// **This exists because raising `MAX_SLOTS` from 129 to 256 silently changed
/// what `.min(MAX_SLOTS)` did.** That clamp had been acting as a *tight* bound
/// on the overshoot: at a ceiling of 129 a document needing its 129th slice
/// doubled from 128 to 256 and was clamped straight back to 129. At a ceiling of
/// 256 the same document gets 256 — 4.29 GB at 2048² where 2.06 GB was asked
/// for, with the old array still alive during the copy, and `ensure_slots` never
/// shrinks so it is permanent for the session. **A legal document with no
/// effects in it reaches this**: 64 layers each with a mask is 128 slices and
/// `begin_float` then asks for the 129th.
///
/// Restoring a 129 clamp was the obvious repair and is wrong — it breaks the
/// moment an effect claims a slice. A budget in bytes does not depend on a
/// ceiling that has already moved twice.
fn grown_capacity(current: u32, needed: u32, slice_bytes: u64) -> u32 {
    let mut capacity = current.max(1);
    while capacity < needed
        && u64::from(capacity)
            .saturating_mul(2)
            .saturating_mul(slice_bytes)
            <= GROWTH_DOUBLING_BUDGET_BYTES
    {
        capacity = capacity.saturating_mul(2);
    }
    // Past the budget, exactly what was asked for. `max` rather than a branch:
    // where doubling did reach `needed` this keeps the amortised capacity.
    capacity.max(needed)
}

const DAB_STRIDE: u64 = std::mem::size_of::<Dab>() as u64;

/// The composite pass, with the blend modes in front of it.
///
/// The two shaders that combine premultiplied colours share one statement of
/// what each mode *is*, by being compiled from one text. CLAUDE.md's rule that
/// `composite.wgsl` and `commit.wgsl` must implement identical blending maths
/// is a rule about a preview and the thing that replaces it; a shared function
/// makes it structural where two hand-written copies of Multiply would leave it
/// to discipline. See `shaders/blend.wgsl`.
///
/// `concat!` rather than a runtime `format!`: this is a `&'static str` compiled
/// into the binary exactly as a lone `include_str!` was.
///
/// **A shader error's line number is not the file's.** naga sees one text, so
/// it counts from the first line of `blend.wgsl` and everything it reports
/// against `composite.wgsl` or `commit.wgsl` is shifted by that file's length.
/// Subtract it before going to look, or the line named will be plausible and
/// wrong — which is worse than one that is obviously out of range.
const BLEND_PRELUDE_COMPOSITE: &str = concat!(
    include_str!("../shaders/blend.wgsl"),
    include_str!("../shaders/composite.wgsl"),
);

/// The commit pass, with the same blend modes in front of it.
///
/// The other half of [`BLEND_PRELUDE_COMPOSITE`]: the preview and the thing
/// that replaces it are compiled from one copy of `blend.wgsl`.
const BLEND_PRELUDE_COMMIT: &str = concat!(
    include_str!("../shaders/blend.wgsl"),
    include_str!("../shaders/commit.wgsl"),
);

/// Per-dab colour, for a smudging stroke only.
///
/// `Rgba16Float` rather than `Rgba8Unorm` because these are **linear** values.
/// Eight bits of linear light bands visibly in the shadows, and a blender
/// working over a dark painting is precisely where it would show. Allocated
/// only when a smudging stroke starts, so an ordinary session never holds it.
const STROKE_COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// A **coloured stamp**'s colour: the tip's own pixels, for the tips that carry
/// any.
///
/// The same convention as [`LAYER_FORMAT`] and chosen for its reasons — sRGB
/// storage, alpha premultiplied in linear light. Eight bits are enough because
/// this *is* eight-bit source data: a `.gbr`, a `.gpb`'s pattern and a PNG in
/// the library all hold a byte a channel, so a wider texture would be storing
/// the same numbers in more space. That is the opposite of
/// [`STROKE_COLOR_FORMAT`], which holds values the engine computed rather than
/// values a file stated.
///
/// sRGB rather than `Rgba8Unorm` for the sake of the *hardware* decode: a
/// bilinear tap on an sRGB texture filters the decoded linear values, which is
/// what the dab pass needs, and encoding the low end costs nothing here where a
/// linear byte would band.
///
/// A coloured tip therefore costs five bytes a texel — one of coverage and four
/// of this — where a mask costs one. It is paid only by the brushes that carry a
/// colour, and at [`TipMask::MAX_SIZE`] it is 20 MB for a stamp nothing else in
/// this codebase would allocate at all.
const TIP_COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// The coverage attachment's blend state.
///
/// `Max` is the whole trick: coverage saturates instead of accumulating, so a
/// stroke crossing itself stays even. Shared by both non-building dab
/// pipelines, so smudging cannot quietly reintroduce the compounding this
/// prevents.
const COVERAGE_TARGET: wgpu::ColorTargetState = wgpu::ColorTargetState {
    format: STROKE_FORMAT,
    blend: Some(wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Max,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Max,
        },
    }),
    write_mask: wgpu::ColorWrites::ALL,
};

/// The coverage attachment's blend state for a **building-up** brush:
/// `a = cov + a(1 - cov)`, which is what one dab compositing over the last
/// means.
///
/// Expressed as `src * One + dst * (1 - src)`. `OneMinusSrc` reads the source
/// *colour*, and coverage lives in the red channel of a single-channel target,
/// so the factor is `1 - cov` per channel — exactly the complement wanted. The
/// alternative, writing coverage into the fragment's alpha as well and using
/// `OneMinusSrcAlpha`, would mean the two paths ran different shader code, and
/// the paint-versus-erase note in `CLAUDE.md` records what that costs: a
/// difference of blending has to live in the blend state or it drifts.
///
/// Nothing downstream changes. The result is still coverage in `0..1` in the
/// same texture, so `composite.wgsl` and `commit.wgsl` are untouched and stroke
/// opacity is still applied exactly once, at commit.
///
/// The floor worth knowing about: the scratch is `R8Unorm`, so a dab weaker
/// than about `1/255` rounds away and a stroke of them never builds, and one
/// only a little above it stalls partway — an increment of `cov * (1 - a)` that
/// falls below half a level stops moving the accumulator at all.
///
/// This is the one place a wider scratch would buy anything, and it was
/// measured rather than argued. Against exact arithmetic, over the whole of
/// Raghukamath's `pack01-drybrush` (stamped along a stroke at its own spacing,
/// 50 dabs deep), `R8Unorm` is at most **3 levels of 255** out and 2.8% of the
/// stroke's pixels are more than one level out. That preset no longer ships —
/// it asks for a paper texture Umber does not carry, see `docs/brushes.md` —
/// so it is now a brush the *importer* produces rather than one in the
/// library; it is the same stamp either way, and Umber's own "Stipple chalk"
/// is the shipped brush that takes this path. The
/// pathological case needs a *constant* faint coverage on one pixel for a
/// hundred dabs, which a bitmap tip cannot produce: the mask slides under the
/// stroke, so a pixel sees a different texel every dab. The mask is itself
/// `R8Unorm` too — filtering can interpolate below `1/255` at the edge of an
/// inked texel, but no *stored* value is that faint, so there is nothing for a
/// wider scratch to recover.
///
/// The other half of the answer is that `R16Unorm` is not available: it needs
/// `Features::TEXTURE_FORMAT_16BIT_NORM`, which Umber does not request, and
/// even with it wgpu guarantees only `storage` usage — not `RENDER_ATTACHMENT`.
/// `R16Float` is the only candidate that is a guaranteed blendable target on
/// `Features::empty()`. See the pressure note in CLAUDE.md.
const COVERAGE_BUILDUP_TARGET: wgpu::ColorTargetState = wgpu::ColorTargetState {
    format: STROKE_FORMAT,
    blend: Some(wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrc,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrc,
            operation: wgpu::BlendOperation::Add,
        },
    }),
    write_mask: wgpu::ColorWrites::ALL,
};

/// The colour attachment's blend state: premultiplied `over`.
///
/// Deliberately *not* `Max`, which is meaningless for colour — it would take
/// the brightest channel wherever a stroke overlapped itself. `over` makes each
/// pixel hold the most recent dabs' colour, which is what produces a smear that
/// trails along the stroke instead of one flat average of everything picked up.
const STROKE_COLOR_TARGET: wgpu::ColorTargetState = wgpu::ColorTargetState {
    format: STROKE_COLOR_FORMAT,
    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
    write_mask: wgpu::ColorWrites::ALL,
};

/// Instance layout, shared by both dab pipelines so they cannot disagree about
/// what a `Dab` looks like in memory.
const DAB_ATTRS: [wgpu::VertexAttribute; 7] = [
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x2,
        offset: 0,
        shader_location: 0,
    },
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32,
        offset: 8,
        shader_location: 1,
    },
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32,
        offset: 12,
        shader_location: 2,
    },
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32,
        offset: 16,
        shader_location: 3,
    },
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x3,
        offset: 20,
        shader_location: 4,
    },
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32,
        offset: 32,
        shader_location: 5,
    },
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32,
        offset: 36,
        shader_location: 6,
    },
];

const DAB_VERTEX_LAYOUT: &[wgpu::VertexBufferLayout] = &[wgpu::VertexBufferLayout {
    array_stride: DAB_STRIDE,
    step_mode: wgpu::VertexStepMode::Instance,
    attributes: &DAB_ATTRS,
}];

/// Side of the square the smudge probe composites into.
///
/// Small on purpose: it is averaged to a single colour, so it only has to be
/// wide enough that one stray pixel cannot dominate what the brush picks up.
const PROBE_SIZE: u32 = 8;
/// Format of every target that is not the window: PNG export, the eyedropper's
/// one pixel, and the smudge probe.
///
/// It must have a **pipeline of its own**. A render pipeline is compiled
/// against its target's format, and the surface is whatever the swapchain
/// offers — `Bgra8Unorm` on a good deal of Windows hardware. Compositing into
/// one of these with the screen's pipeline is a validation error that kills the
/// process, and it is invisible on any machine whose surface happens to be
/// `Rgba8Unorm`.
const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

const PROBE_FORMAT: wgpu::TextureFormat = OFFSCREEN_FORMAT;

/// Side of a layer thumbnail's target, in texels. See
/// [`umber_core::thumbnail`], which owns the number and the reason for it.
const THUMB_SIZE: u32 = umber_core::thumbnail::SIZE;
/// Bytes per row of a thumbnail readback. `THUMB_SIZE` RGBA texels is exactly
/// the 256-byte copy alignment, so there is no padding to stride over — which
/// is one of the two reasons that size was chosen.
const THUMB_ROW_BYTES: u32 = THUMB_SIZE * 4;
const _: () = assert!(THUMB_ROW_BYTES.is_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT));
/// Rows in a texture-to-buffer copy must be a multiple of 256 bytes, and eight
/// RGBA pixels are 32 — so each row is padded and the reader strides over it.
const PROBE_ROW_BYTES: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
/// Probes in flight at once. Two is enough to keep a sample arriving every
/// frame while never waiting on one: while the first is being mapped the second
/// is being rendered.
const PROBE_SLOTS: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ProbeState {
    /// Free to be handed to a new probe.
    Idle,
    /// A copy has been recorded into it but not yet mapped.
    Rendering,
    /// `map_async` is outstanding.
    Mapping,
}

/// `map_async` has not called back yet.
const PROBE_PENDING: u8 = 0;
/// The buffer is mapped and holds a sample.
const PROBE_MAPPED: u8 = 1;
/// The map failed. Nothing to read, and nothing to unmap either.
const PROBE_FAILED: u8 = 2;

/// One slot in the smudge probe's rotation: a staging buffer and where it is
/// up to.
struct Probe {
    buffer: wgpu::Buffer,
    state: ProbeState,
    /// One of the `PROBE_*` constants, written by the map callback — which runs
    /// on whichever thread polls the device, hence the atomic. Tri-state rather
    /// than a flag because a *failed* map leaves the buffer unmapped, and
    /// unmapping it anyway is as wrong as never returning the slot to service.
    outcome: Arc<AtomicU8>,
    /// The stroke this sample was taken for has ended, so whatever comes back
    /// must be thrown away rather than smeared into the next stroke.
    stale: bool,
}

/// Average a probe readback into one linear RGBA.
///
/// The composite's export path writes **sRGB** with straight alpha, so the
/// decode happens here. Averaging the sRGB bytes directly would be the classic
/// mistake — the mean of two gamma-encoded values is not the gamma encoding of
/// their mean, and a blender working across an edge would pick up a colour
/// lighter than either side of it.
///
/// Colour is weighted by coverage so that transparent pixels do not drag the
/// average towards whatever happens to sit in their unused colour channels.
fn average_probe(bytes: &[u8]) -> [f32; 4] {
    let decode = |b: u8| {
        let c = b as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };

    let mut rgb = [0.0f32; 3];
    let mut alpha = 0.0f32;
    let mut weight = 0.0f32;
    for y in 0..PROBE_SIZE {
        let row = (y * PROBE_ROW_BYTES) as usize;
        for x in 0..PROBE_SIZE {
            let i = row + x as usize * 4;
            let Some(px) = bytes.get(i..i + 4) else {
                continue;
            };
            let a = px[3] as f32 / 255.0;
            alpha += a;
            weight += a;
            for c in 0..3 {
                rgb[c] += decode(px[c]) * a;
            }
        }
    }

    let n = (PROBE_SIZE * PROBE_SIZE) as f32;
    if weight <= 0.0 {
        return [0.0, 0.0, 0.0, 0.0];
    }
    [rgb[0] / weight, rgb[1] / weight, rgb[2] / weight, alpha / n]
}

// --- the whole-document capture --------------------------------------------

/// How much of a mapped capture buffer is read per frame.
///
/// Reading a mapped staging buffer reads uncached memory: a whole 16 MB layer
/// measured about 5 ms, which is a third of a 60 Hz frame spent on something
/// the user did not ask for. Four megabytes is comfortably under a millisecond
/// and costs only a few more frames — see [`Capture::copy_chunk`].
const CAPTURE_CHUNK_BYTES: usize = 4 << 20;

/// Where a [`Capture`]'s one staging buffer has got to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum StepState {
    /// Free. Nothing is outstanding on the GPU, so the next copy can be
    /// recorded into it — or the whole capture abandoned.
    Waiting,
    /// A copy has been recorded but `map_async` has not been called.
    Rendering,
    /// `map_async` is outstanding.
    Mapping,
}

/// A whole document on its way to the CPU, one buffer at a time.
///
/// The blocking readback [`CanvasRenderer::read_layer_rect`] performs is
/// acceptable once, at pointer-up, on an explicit Save — and is exactly wrong
/// on a timer, which is what an autosave is. So this is the smudge probe's idea
/// at document scale: a copy is recorded into the frame's own encoder,
/// `map_async` is called after that frame is submitted, and the bytes are
/// collected on some later frame by a poll that never waits.
///
/// **One layer in flight at a time, through one reused buffer.** Recording
/// every copy at once would cost the same in CPU calls, and would be wrong
/// twice over: the GPU would move the whole document in a single frame — over a
/// hundred megabytes on a 2048² stack — and the maps would then come home
/// together, landing every one of those memcpys in one frame. Both are exactly
/// the hitch this exists to avoid. Serialised, a frame pays either a recording
/// (microseconds) or one layer's memcpy, and the staging cost is one buffer
/// rather than one per layer. The price is that the capture takes a couple of
/// frames per layer, which for a five-minute timer is no price at all.
/// How many rows of a `padded`-byte-wide copy fit in one staging buffer.
///
/// A document can be far larger than the biggest buffer the device will make.
/// `Limits::downlevel_defaults` caps `max_buffer_size` at 256 MB and
/// `using_resolution` raises only the texture dimensions, so a 10000² canvas —
/// perfectly paintable, 400 MB of RGBA — could be drawn on and then not read
/// back: `create_buffer` refuses the size, and a validation error aborts the
/// process. That is exactly what happened, on the undo capture at the end of
/// the first stroke.
///
/// Raising the limit instead would be the wrong fix twice over: it would break
/// the rule that a desktop build may not depend on what a mobile GPU refuses,
/// and 256 MB is a limit real hardware has. So every readback goes a band of
/// rows at a time, and this decides how tall a band is.
///
/// Returns at least one row. A single row wider than the whole limit would
/// still be refused, but that needs a canvas 67 million pixels across — far
/// beyond `max_texture_dimension_2d`, which is checked long before this.
fn band_rows(limit: u64, padded: u32, height: u32) -> u32 {
    let rows = limit / u64::from(padded).max(1);
    rows.min(u64::from(height)).max(1) as u32
}

struct Capture {
    size: UVec2,
    /// Bytes per row of the copy, rounded up to the copy alignment.
    padded: u32,
    /// Which texture-array slices to read, in the order the caller asked for.
    slots: Vec<u32>,
    /// The stack as the flattened preview should composite it.
    draws: Vec<LayerDraw>,
    /// The one staging buffer, allocated on the first step and reused.
    buffer: Option<wgpu::Buffer>,
    /// Rows per staging buffer, from [`band_rows`]. A whole layer where the
    /// device's limit allows it, which is every ordinary document; a large
    /// canvas is read a band at a time instead. Set on the first
    /// [`CanvasRenderer::drive_capture`], which is the first place with a
    /// device to ask.
    band: u32,
    /// The first row of the band in flight, within the step's layer.
    row: u32,
    /// One of the `PROBE_*` constants, for the reason [`Probe::outcome`] is.
    outcome: Arc<AtomicU8>,
    state: StepState,
    /// The step in flight, or the next to be recorded. `slots.len()` is the
    /// flattened preview, which goes last.
    step: usize,
    /// One entry per step, filled in as each comes home.
    results: Vec<Option<Vec<u8>>>,
    /// The step in flight, as far as it has been copied out of the mapped
    /// buffer. See [`Capture::copy_chunk`].
    partial: Option<Vec<u8>>,
    /// The offscreen target the preview is drawn into, held until its copy has
    /// been submitted.
    merged_target: Option<wgpu::Texture>,
    /// The document has gone, or changed shape, so whatever comes home is
    /// worthless. The buffer stays where it is until its map has settled — see
    /// [`CanvasRenderer::reset_probes`] for why handing one back early is a
    /// crash rather than an untidiness.
    abandoned: bool,
    /// A map failed, so nothing can be assembled. The job is dropped once the
    /// buffer has settled.
    failed: bool,
}

impl Capture {
    /// One per layer, plus the flattened preview.
    fn steps(&self) -> usize {
        self.slots.len() + 1
    }

    /// True once every step has its bytes.
    fn complete(&self) -> bool {
        self.step >= self.steps()
    }

    /// True once nothing is outstanding on the GPU, so the job can be dropped.
    fn settled(&self) -> bool {
        self.state == StepState::Waiting
    }

    /// The rows the band in flight covers, as `[first, last)`.
    fn band_span(&self) -> (usize, usize) {
        let first = self.row as usize;
        let last = (first + self.band.max(1) as usize).min(self.size.y as usize);
        (first, last)
    }

    /// True once every band of this step has been copied out.
    fn step_done(&self) -> bool {
        self.partial.as_ref().map(Vec::len).unwrap_or(0)
            >= (self.size.x * 4) as usize * self.size.y as usize
    }

    /// Take the next slice of rows out of the mapped buffer. Returns true once
    /// the band in flight is out of it.
    ///
    /// Bounded because reading a mapped staging buffer reads *uncached* memory
    /// — a 16 MB layer measured about 5 ms on a mid-range discrete card, which
    /// is a third of a frame at 60 Hz for something the user did not ask for.
    /// Split into [`CAPTURE_CHUNK_BYTES`] pieces it is under a millisecond, and
    /// the capture merely takes a few more frames. On a five-minute timer that
    /// is not a cost at all.
    ///
    /// By rows rather than by bytes, because the copy's rows are padded to the
    /// alignment: chunking by rows makes the padding fall out for free.
    fn copy_chunk(&mut self) -> bool {
        let row = (self.size.x * 4) as usize;
        let height = self.size.y as usize;
        let (band_first, band_last) = self.band_span();
        let buffer = self.buffer.as_ref().expect("a mapped step has its buffer");
        // Only what the band actually wrote. The buffer is a full band long and
        // the last band is usually short, so mapping all of it would read the
        // previous band's rows back out of the tail.
        let mapped = buffer
            .slice(..(self.padded as u64) * ((band_last - band_first) as u64))
            .get_mapped_range();

        let out = self
            .partial
            .get_or_insert_with(|| Vec::with_capacity(row * height));
        // Absolute, because `partial` accumulates across every band of the step.
        let from = out.len() / row;
        let rows = (CAPTURE_CHUNK_BYTES / self.padded as usize).max(1);
        let to = (from + rows).min(band_last);
        for y in from..to {
            // Band-relative: row `band_first` of the layer is row 0 of the
            // buffer.
            let start = (y - band_first) * self.padded as usize;
            out.extend_from_slice(&mapped[start..start + row]);
        }
        to >= band_last
    }
}

/// Everything a document needs written down, read back without a stall.
///
/// `layers` are in **layer-texture form** — sRGB with alpha premultiplied in
/// linear space — which is what `umber_core::docformat::SaveLayer::pixels`
/// wants. `merged` is straight-alpha sRGB, as `SaveDocument::merged` wants.
/// Both come from the same passes the screen uses, so an autosaved file cannot
/// disagree with what was on screen.
pub struct DocumentCapture {
    pub size: UVec2,
    /// One buffer per slot asked for, in that order.
    pub layers: Vec<Vec<u8>>,
    pub merged: Vec<u8>,
}

// --- layer thumbnails -------------------------------------------------------

/// Which of a thumbnail's two passes is in flight.
///
/// They are the *same* pass with `reduce` flipped — see `thumbnail.wgsl`. The
/// second cannot be recorded until the first has come home, because what it
/// draws is decided by what the first found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ThumbPhase {
    /// Reducing the whole slice to the greatest alpha per cell, to find where
    /// the layer's content is.
    Bounds,
    /// Reducing that region to a mean, which is the picture.
    Picture,
}

/// One layer thumbnail on its way to the CPU.
///
/// Built like the smudge probe rather than like the autosave's capture: two
/// small copies, a `map_async` after the frame that recorded each, and a
/// collection on some later frame by a poll that never waits. Nothing here may
/// block, because the layer list is redrawn every frame and the blocking
/// readbacks — `read_layer_rect` and `read_layer_pieces` — are explicitly
/// reserved for a Save and for a pointer-up.
///
/// One at a time, for the reason [`Capture`] is one at a time: the cost is a
/// couple of frames of latency on something nobody is waiting for, and the
/// alternative is one staging buffer per layer.
struct ThumbJob {
    slot: u32,
    /// [`CanvasRenderer::slot_revision`] when the job began. Handed back with
    /// the picture so the caller can tell a thumbnail of the layer as it is now
    /// from one of the layer as it was two strokes ago.
    revision: u64,
    phase: ThumbPhase,
    /// The region the picture pass draws, from `umber_core::thumbnail::framed`.
    /// `None` until the bounds pass has come home.
    region: Option<umber_core::Rect>,
    state: StepState,
    /// One of the `PROBE_*` constants, for the reason [`Probe::outcome`] is.
    outcome: Arc<AtomicU8>,
    /// The layer was written to, or the document has gone, so whatever comes
    /// home describes a picture that is no longer there. Marked rather than
    /// dropped, exactly as a probe is — a buffer awaiting a map is still the
    /// GPU's, and handing it back early is a validation error and therefore an
    /// abort. See [`CanvasRenderer::reset_probes`].
    abandoned: bool,
}

/// A layer thumbnail, as the interface wants it.
pub struct Thumbnail {
    pub slot: u32,
    /// The revision the job began at. A caller holding a newer one knows to ask
    /// again rather than to draw this.
    pub revision: u64,
    /// Straight-alpha sRGB, `SIZE` square — or empty where the layer holds
    /// nothing at all, which is a state the list draws rather than a failure.
    pub rgba: Vec<u8>,
}

impl Thumbnail {
    /// True where the layer had no non-transparent pixel to show.
    pub fn is_empty(&self) -> bool {
        self.rgba.is_empty()
    }
}

/// One layer's contribution to the composite, in stack order.
#[derive(Clone, Copy, Debug, Default)]
pub struct LayerDraw {
    /// Texture-array slice holding the pixels.
    pub slot: u32,
    pub opacity: f32,
    /// Matches `umber_core::BlendMode::index`.
    pub blend: u32,
    pub visible: bool,
    /// Slice holding this layer's mask, when it has one. Another slice of the
    /// same array — see `umber_core::layer`'s module docs for why.
    ///
    /// `None` is the exact identity in the shader: the mask factor is 1.0 and
    /// nothing is sampled that matters.
    pub mask: Option<u32>,
    /// Bounded by the alpha of the nearest unclipped layer below.
    pub clipped: bool,
}

/// How the stroke in the scratch surface should look.
///
/// Preview and commit must be handed the *same* style — they implement the same
/// blending maths, and any disagreement shows up as the stroke jumping at
/// pointer-up. Passing them together makes that hard to get wrong.
#[derive(Clone, Copy, Debug)]
pub struct StrokeStyle {
    pub color: Color,
    /// Applied once, on commit — never folded into per-dab coverage.
    pub opacity: f32,
    pub mode: BrushMode,
    /// How the finished stroke combines with the layer it lands on:
    /// `umber_core::Brush::blend`, snapshotted with everything else here.
    ///
    /// It lives in this struct for the reason everything else in it does. The
    /// preview computes it in `composite.wgsl` and the commit computes it in
    /// `commit.wgsl`, out of one shared `composite_over` — so the two cannot
    /// disagree about what Multiply is, but they still have to be told the same
    /// mode, and being handed one `StrokeStyle` is what guarantees that.
    ///
    /// [`BlendMode::Normal`] is the path every stroke took before brushes had
    /// one, and it stays exactly as it was: the fixed-function blender does it,
    /// with no backdrop copy and no extra pass.
    ///
    /// Ignored when [`StrokeStyle::mode`] is [`BrushMode::Erase`] — an eraser
    /// deposits no colour for a mode to combine — and
    /// `umber_core::Brush::blend_applies` is where that is decided, so nothing
    /// here has to hold both in mind.
    pub blend: BlendMode,
    /// The stroke deposits a colour per dab — it smudges — so `color` is only
    /// the fallback and the real colour comes from the stroke's colour scratch.
    ///
    /// Must match what was passed to [`CanvasRenderer::draw_dabs`] for the same
    /// stroke. Preview and commit both read it, which is what keeps them from
    /// disagreeing about where the colour came from.
    pub per_dab_color: bool,
    /// The stroke is landing in the active layer's **mask** rather than in its
    /// pixels.
    ///
    /// The switch lives here, and only here, for the reason everything else in
    /// this struct does: preview and commit are handed the same `StrokeStyle`,
    /// so they cannot disagree about which of the two a stroke is going into.
    /// The slice it commits to is the one passed to
    /// [`CanvasRenderer::commit_stroke`] — a mask is an ordinary slice, so the
    /// commit pass needs no variant of its own; what this flag decides is where
    /// `composite.wgsl` blends the *preview*.
    pub on_mask: bool,
}

impl Default for StrokeStyle {
    fn default() -> Self {
        Self {
            color: Color::BLACK,
            opacity: 1.0,
            mode: BrushMode::Paint,
            blend: BlendMode::Normal,
            per_dab_color: false,
            on_mask: false,
        }
    }
}

/// How the dab pass should blend, for the whole of one stroke.
///
/// Both flags choose a *pipeline*, and a pipeline cannot be changed halfway
/// through a stroke without the dabs already in the scratch having been drawn
/// under the other rule. Hence one struct rather than two loose booleans:
/// whatever a stroke starts with, it finishes with.
///
/// Neither flag reaches [`CompositeParams`] or [`CanvasRenderer::commit_stroke`]
/// except through [`StrokeStyle::per_dab_color`], which they must agree with —
/// build-up is invisible downstream by design, because it changes only how
/// coverage arrives in the scratch and not what the scratch means.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DabStyle {
    /// Record a colour per dab as well as coverage. Must equal the
    /// [`StrokeStyle::per_dab_color`] handed to composite and commit.
    pub per_dab_color: bool,
    /// Accumulate coverage rather than taking a `max` of it. See
    /// [`COVERAGE_BUILDUP_TARGET`].
    pub build_up: bool,
}

impl DabStyle {
    /// Index into the pipeline matrix. Bit 0 is colour, bit 1 is build-up.
    fn index(self) -> usize {
        usize::from(self.per_dab_color) | usize::from(self.build_up) << 1
    }
}

/// The pixels a floating transform starts from, and where they sit.
///
/// One struct for both ways in, because everything after the first submission is
/// identical: a lift and a paste differ only in where the pixels come from and
/// whether the layer beneath keeps them.
pub struct FloatSource<'a> {
    /// The layer the float sits over. Its contents are the backdrop the float
    /// is previewed against, and a commit lands here.
    pub slot: u32,
    /// Where the floating pixels are before anything has been dragged, in
    /// document space.
    pub rect: PixelRect,
    /// Pixels to put down, in layer-texture form (sRGB, alpha premultiplied in
    /// linear space) and `rect`-sized. `None` **lifts** them out of `slot`
    /// instead, leaving a hole where they were.
    pub pixels: Option<&'a [u8]>,
    /// Clips both the lift and the hole it leaves. Ignored for a paste, which
    /// puts down exactly what it was given.
    pub mask: Option<&'a Selection>,
}

/// Where a floating transform's pixels have been dragged to.
#[derive(Clone, Copy, Debug)]
pub struct FloatParams {
    /// Destination document pixel back to where it came from — see
    /// `umber_core::transform`. The resampler walks the destination, so this is
    /// the direction the shader needs.
    pub inverse: Affine,
    /// The rectangle the result lands in, or `None` when the drag has carried
    /// it clean off the canvas. `None` is not "nothing to do": the previous
    /// destination still has to be restored.
    pub dest: Option<PixelRect>,
}

/// A floating region: pixels lifted out of a layer or pasted onto one, being
/// moved about before they are put down.
///
/// # How the preview cannot disagree with the commit
///
/// The stroke pipeline has two implementations of one blend — `composite.wgsl`
/// previews and `commit.wgsl` bakes — and CLAUDE.md is emphatic about keeping
/// them in step. This has none, and it is arranged that way rather than
/// disciplined that way:
///
/// * [`Float::base`] holds the layer as it will be *underneath* the float — the
///   original pixels, with the lifted region taken out. It is built once.
/// * The preview is `base` restored over the damaged rectangle, then the
///   transformed source drawn over it, into a spare slice of the layer array.
///   The composite pass is handed that slice **in place of the layer's own**,
///   so it composites a floating transform without knowing there is one: no new
///   uniform, no new branch, not a line of `composite.wgsl` touched.
/// * The commit is [`CanvasRenderer::render_float`] again, byte for byte, with
///   the layer's own slice as the target instead of the spare one.
///
/// So the preview and the committed result are not two renderings that have to
/// agree. They are the same two commands run twice, and the second one is the
/// first with a different destination.
struct Float {
    /// The layer this float sits over, and where a commit lands.
    layer_slot: u32,
    /// The layer-array slice the composite pass draws in place of
    /// [`Float::layer_slot`].
    preview_slot: u32,
    /// The layer with the lifted region removed. Canvas-sized.
    base: wgpu::Texture,
    /// The floating pixels at identity: canvas-sized, zero outside the region
    /// that was lifted or pasted. Held so it outlives the bind group.
    #[allow(dead_code)]
    source: wgpu::Texture,
    /// The selection's coverage, or a 1x1 placeholder. Snapshotted here rather
    /// than read off the renderer's stroke-path mask, so the two features
    /// cannot reach into each other. Held so it outlives the bind group.
    #[allow(dead_code)]
    mask: wgpu::Texture,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    /// Where the previous preview landed. The next one restores this as well as
    /// its own rectangle — without it the picture leaves a trail behind the
    /// drag.
    last_dest: Option<PixelRect>,
}

/// Where a smudging brush should sample the canvas, and what it is painting.
///
/// The stack and stroke are the same ones the screen composite is given, and
/// deliberately so: the probe reuses the composite pass, so a blender picks up
/// exactly what the painter can see under the brush — including the wet stroke.
#[derive(Clone, Copy)]
pub struct ProbeParams<'a> {
    /// Bottom-to-top, as [`CompositeParams::layers`].
    pub layers: &'a [LayerDraw],
    pub active_index: u32,
    pub stroke: StrokeStyle,
    /// Centre of the sample, in document pixels.
    pub doc_point: Vec2,
    /// Radius of the patch to average, in document pixels.
    pub radius: f32,
}

/// Everything the composite pass needs for a frame.
pub struct CompositeParams<'a> {
    pub camera: &'a Camera,
    /// Screen point the camera's centre sits on, in physical pixels. This is
    /// the middle of the *canvas region*, not the window — panels take a bite
    /// out of the window and the document should sit in what remains.
    pub pivot: Vec2,
    /// Bottom-to-top.
    pub layers: &'a [LayerDraw],
    /// Stack position (not slot) receiving the in-progress stroke.
    pub active_index: u32,
    pub stroke: StrokeStyle,
    /// Surround colour, display-space RGB.
    pub backdrop: [f32; 3],
    /// Render for file output rather than for the screen.
    pub export: bool,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct DabUniforms {
    doc_size: [f32; 2],
    /// The tip's proportions, longer side normalised to 1. `[1.0, 1.0]` with no
    /// tip, which is the exact identity in the shader.
    tip_scale: [f32; 2],
    /// Non-zero when a real tip texture is bound. Scalar padding, not a vec2 —
    /// see the uniform-layout note in CLAUDE.md.
    use_tip: u32,
    /// How hard the paper bites, 0..1. Zero is the exact identity.
    grain_strength: f32,
    /// Side of one grain tile in document pixels.
    grain_scale: f32,
    _pad: f32,
    /// Where the selection mask is mapped to, in document pixels.
    sel_min: [f32; 2],
    /// Its size. `[1.0, 1.0]` with no selection, so the shader's divide is
    /// never by zero even though its result is thrown away.
    sel_size: [f32; 2],
    /// Non-zero when a real mask is bound. Unlike the tip and the grain this
    /// cannot be folded into a placeholder — see the WGSL struct.
    use_selection: u32,
    /// Non-zero when the tip carries a colour of its own. Took the place of one
    /// of the three padding words already here, so the block is the size it
    /// always was.
    use_tip_color: u32,
    /// Scalar padding, not a vec3: see the uniform-layout note in CLAUDE.md.
    _pad2: f32,
    _pad3: f32,
}

impl DabUniforms {
    /// The uniforms for a document with no tip, no grain and no selection:
    /// every factor the shader multiplies by is one.
    fn plain(doc_size: UVec2) -> Self {
        Self {
            doc_size: [doc_size.x as f32, doc_size.y as f32],
            tip_scale: [1.0, 1.0],
            use_tip: 0,
            grain_strength: 0.0,
            grain_scale: 1.0,
            _pad: 0.0,
            sel_min: [0.0, 0.0],
            sel_size: [1.0, 1.0],
            use_selection: 0,
            use_tip_color: 0,
            _pad2: 0.0,
            _pad3: 0.0,
        }
    }
}

/// Mirrors `View` in `composite.wgsl`, byte for byte.
///
/// The arithmetic, because it is the one uniform here large enough for the
/// answer to be in doubt. Four `vec2<f32>` (32) + three `vec4<f32>` (48) +
/// eight scalars (32) = **112 bytes** of head, which is 16-aligned, so
/// `layers` starts there with no padding inserted. Each array is
/// `MAX_DRAWS × 16`, so the whole block is `112 + 2 × 191 × 16` = **6224
/// bytes**, against `downlevel_defaults`' `max_uniform_buffer_binding_size` of
/// 16 KiB. `the_view_uniform_fits_the_smallest_binding_a_device_must_offer`
/// measures all of it rather than trusting the sum written here.
///
/// It was 2160 bytes while the arrays held 64; the growth is the effect-draw
/// budget and is paid in bytes uploaded per frame, not per fragment.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ViewUniforms {
    scale: [f32; 2],
    offset: [f32; 2],
    doc_size: [f32; 2],
    pivot: [f32; 2],
    stroke_color: [f32; 4],
    backdrop: [f32; 4],
    /// Premultiplied linear; see the WGSL struct. `vec4` is 16-aligned on both
    /// sides and sits on a 16-byte boundary here, so this insertion moves
    /// nothing after it.
    background: [f32; 4],
    layer_count: u32,
    stroke_mode: u32,
    active_index: u32,
    checker: f32,
    is_export: u32,
    per_dab_color: u32,
    /// The stroke in flight is going into the active layer's mask.
    stroke_on_mask: u32,
    /// The brush's blend mode, in [`BlendMode::index`]'s numbering. Took the
    /// place of the padding word that was here, so the block is the size it
    /// always was — see the WGSL struct.
    stroke_blend: u32,
    /// (opacity, blend, slot, visible) per draw.
    layers: [[f32; 4]; MAX_DRAWS],
    /// (mask slot, has mask, clipped, unused) per draw. See the WGSL struct for
    /// why this is a second array rather than bits in the first.
    extra: [[f32; 4]; MAX_DRAWS],
}

/// Mirrors `Xf` in `transform.wgsl`. Every member is a `vec2<f32>`, which is
/// 8-aligned on both sides, so the packing is the obvious one — see the
/// uniform-layout note in CLAUDE.md for why a `mat2x2` here would not be.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TransformUniforms {
    rect_min: [f32; 2],
    rect_max: [f32; 2],
    doc_size: [f32; 2],
    inv_x: [f32; 2],
    inv_y: [f32; 2],
    inv_t: [f32; 2],
    mask_min: [f32; 2],
    mask_size: [f32; 2],
    use_mask: u32,
    /// Scalar padding, not a vec3: see the uniform-layout note in CLAUDE.md.
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

/// Mirrors `Commit` in `commit.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CommitUniforms {
    rect_min: [f32; 2],
    rect_max: [f32; 2],
    doc_size: [f32; 2],
    _pad0: [f32; 2],
    color: [f32; 4],
    mode: u32,
    per_dab_color: u32,
    /// [`BlendMode::index`]. Took one of the two padding words already here, so
    /// the block is the size it always was. Scalars rather than a `vec3`: see
    /// the uniform-layout note in CLAUDE.md.
    blend: u32,
    _pad2: u32,
}

/// Mirrors `Flip` in `flip.wgsl`.
///
/// `vec2<u32>` is 8-aligned on both sides and the two scalars after it pack
/// into the same 16 bytes, so the block is 16 wide with no surprises. Scalar
/// padding, not a `vec3`: see the uniform-layout note in CLAUDE.md.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FlipUniforms {
    doc_size: [u32; 2],
    axis: u32,
    _pad: u32,
}

/// Mirrors `Thumb` in `thumbnail.wgsl`.
///
/// Every member is a `vec2` or a scalar, so both sides pack to 48 bytes with no
/// alignment surprise. Scalar padding, not a `vec2`: see the uniform-layout note
/// in CLAUDE.md.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ThumbUniforms {
    src_min: [f32; 2],
    src_size: [f32; 2],
    dest: [u32; 2],
    layer_size: [u32; 2],
    slot: u32,
    reduce: u32,
    _pad0: u32,
    _pad1: u32,
}

/// The layer texture array and the views onto it.
struct LayerStore {
    texture: wgpu::Texture,
    /// Sampled by the composite pass.
    array_view: wgpu::TextureView,
    /// One per slice, used as render targets by commit and clear.
    slot_views: Vec<wgpu::TextureView>,
    /// The same slices seen as [`LAYER_FORMAT_LINEAR`], for the flip pass and
    /// nothing else. Built here rather than per flip because a view is cheap to
    /// hold and the alternative is allocating one per slice per command.
    raw_slot_views: Vec<wgpu::TextureView>,
    capacity: u32,
}

impl LayerStore {
    fn new(device: &wgpu::Device, size: UVec2, capacity: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("umber-layers"),
            size: wgpu::Extent3d {
                width: size.x,
                height: size.y,
                depth_or_array_layers: capacity,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: LAYER_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            // The flip pass needs to see these bytes without the transfer
            // function on the way in or out — see [`LAYER_FORMAT_LINEAR`].
            // Declared here because a view of a format the texture was not
            // created for is a validation error, not a conversion.
            view_formats: &[LAYER_FORMAT_LINEAR],
        });

        let array_view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("umber-layers-array"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });

        let slot_views = (0..capacity)
            .map(|i| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("umber-layer-slot"),
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: i,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();

        let raw_slot_views = (0..capacity)
            .map(|i| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("umber-layer-slot-raw"),
                    format: Some(LAYER_FORMAT_LINEAR),
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: i,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();

        Self {
            texture,
            array_view,
            slot_views,
            raw_slot_views,
            capacity,
        }
    }
}

/// Everything that does not depend on the document: compiled shaders,
/// pipelines, bind group layouts and the sampler.
///
/// Split out so a second open document can have its own textures without its
/// own shaders. Every field is a reference-counted wgpu handle, so cloning this
/// is a few atomic increments where rebuilding it is three shader compilations
/// and four pipeline creations — a stall the user would pay on the frame they
/// open a document. See [`CanvasRenderer::for_document`].
#[derive(Clone)]
struct Shared {
    sampler: wgpu::Sampler,
    /// Repeats where [`Shared::sampler`] clamps. A paper tile has to wrap — it
    /// covers the whole document — and a tip stretched over its dab must not.
    grain_sampler: wgpu::Sampler,

    /// The four dab pipelines, indexed by [`DabStyle::index`].
    ///
    /// Two independent binary choices, so four — but written once and built by
    /// a loop, because they differ in exactly two fields and four copies of a
    /// pipeline descriptor is four places for the vertex layout to drift. The
    /// coloured pair carry a second attachment that nearly every stroke does
    /// not want; the building pair swap the coverage target's blend state. All
    /// four share one shader module and one pipeline layout.
    dab_pipelines: [wgpu::RenderPipeline; 4],
    dab_layout: wgpu::BindGroupLayout,

    composite_pipeline: wgpu::RenderPipeline,
    /// The same pass compiled for [`OFFSCREEN_FORMAT`], for export, the
    /// eyedropper and the smudge probe. See that constant for why the screen's
    /// pipeline cannot be reused.
    composite_offscreen_pipeline: wgpu::RenderPipeline,
    composite_layout: wgpu::BindGroupLayout,

    commit_layout: wgpu::BindGroupLayout,
    commit_pipeline: wgpu::RenderPipeline,
    commit_erase_pipeline: wgpu::RenderPipeline,
    /// A commit for a brush whose blend mode is not Normal.
    ///
    /// Its own layout because it needs a fifth binding — a copy of the layer
    /// under the piece, which the pass cannot sample out of its own attachment
    /// — and because its uniform is bound with a dynamic offset, one block per
    /// piece. See [`CanvasRenderer::commit_blended`].
    commit_blend_layout: wgpu::BindGroupLayout,
    commit_blend_pipeline: wgpu::RenderPipeline,

    /// Mirrors one layer slice. See `flip.wgsl` for why it is its own pass
    /// rather than a copy or a use of the transform resampler.
    flip_layout: wgpu::BindGroupLayout,
    flip_pipeline: wgpu::RenderPipeline,

    /// Reduces a rectangle of one slice to a 64-square, for the layer list.
    /// One pipeline for both of a thumbnail's passes — they differ by a uniform
    /// and nothing else. See `thumbnail.wgsl`.
    thumb_layout: wgpu::BindGroupLayout,
    thumb_pipeline: wgpu::RenderPipeline,

    transform_layout: wgpu::BindGroupLayout,
    /// `dst * cov` — takes the selected pixels into the floating copy.
    transform_keep_pipeline: wgpu::RenderPipeline,
    /// `dst * (1 - cov)` — leaves the hole behind in the base.
    transform_take_pipeline: wgpu::RenderPipeline,
    /// The resampler: the floating copy through the inverse transform,
    /// premultiplied source-over.
    transform_draw_pipeline: wgpu::RenderPipeline,
}

pub struct CanvasRenderer {
    doc_size: UVec2,
    /// The document background, premultiplied linear.
    ///
    /// A field rather than a [`CompositeParams`] member because it belongs to
    /// the *document*, and a renderer already is one document's. That is not
    /// tidiness: `export_rgba`, `pick_colour` and `probe_canvas` all build
    /// their own `CompositeParams`, and a per-frame parameter would have to be
    /// threaded into each of them — three more places for the export to stop
    /// matching the screen. Held here, they cannot disagree.
    background: [f32; 4],
    shared: Shared,

    layers: LayerStore,
    #[allow(dead_code)]
    stroke: wgpu::Texture,
    stroke_view: wgpu::TextureView,
    /// Per-dab colour, or a 1x1 placeholder until a smudging stroke first needs
    /// it. Held so it outlives the bind groups referencing it.
    stroke_color: wgpu::Texture,
    stroke_color_view: wgpu::TextureView,
    has_stroke_color: bool,
    /// Staging buffers for the smudge probe, rotated so a stroke never waits on
    /// the GPU to tell it what colour it is passing over.
    probes: Vec<Probe>,
    /// The autosave's whole-document readback, if one is in flight. At most one
    /// per document: a second would double the staging cost for a job that is
    /// already going to be repeated in five minutes.
    capture: Option<Capture>,
    /// The largest staging buffer this device will create, in bytes.
    ///
    /// Taken from the device rather than assumed, and honoured by every
    /// readback here — see [`band_rows`]. Held as a field so a test can lower
    /// it and drive the banded path on a document small enough to check by
    /// hand; on a real device it would take a 8192² canvas to reach.
    readback_limit: u64,

    dab_bind_group: wgpu::BindGroup,
    dab_uniforms: wgpu::Buffer,
    dab_instances: wgpu::Buffer,
    dabs_this_frame: u32,
    /// The bitmap tip, or a 1x1 placeholder. Held so it outlives the bind
    /// group that references it.
    tip: wgpu::Texture,
    has_tip: bool,
    /// A coloured stamp's colour, or a 1x1 placeholder. Held so it outlives the
    /// bind group. Allocated only for the tips that carry one — see
    /// [`TIP_COLOR_FORMAT`] for what it costs when they do.
    tip_color: wgpu::Texture,
    tip_color_view: wgpu::TextureView,
    /// Which mask is in that texture, so [`CanvasRenderer::set_tip`] can tell
    /// "the same brush again" from "a different brush".
    tip_mask: Option<Arc<TipMask>>,
    /// The paper tile, or a 1x1 placeholder. Held so it outlives the bind group.
    grain: wgpu::Texture,
    grain_view: wgpu::TextureView,
    /// Which tile is in that texture, compared by `Arc` identity for exactly
    /// the reason [`CanvasRenderer::tip_mask`] is.
    grain_tile: Option<Arc<TipMask>>,
    /// The selection mask, or a 1x1 placeholder. Held so it outlives the bind
    /// group.
    selection: wgpu::Texture,
    selection_view: wgpu::TextureView,
    /// Which selection is in that texture, by `Arc` identity — the same check
    /// and the same reason as [`CanvasRenderer::tip_mask`]: comparing the
    /// coverage would cost more than the upload it saves.
    selection_mask: Option<Arc<Selection>>,
    /// The dab pass's uniforms, held rather than rebuilt: the tip and the grain
    /// are set independently, and reconstructing the block from one of them
    /// would clear the other's fields.
    dab_state: DabUniforms,
    /// Strength and tile size, so that changing only these does not rebuild a
    /// bind group or re-upload a texture.
    grain_params: (f32, f32),

    composite_bind_group: wgpu::BindGroup,
    view_uniforms: wgpu::Buffer,

    commit_bind_group: wgpu::BindGroup,
    commit_uniforms: wgpu::Buffer,

    /// The floating transform in progress, if there is one. Everything it owns
    /// is allocated when the gesture starts and given back when it ends —
    /// two canvas-sized textures and a slice of the layer array is not
    /// something to hold for a session in case somebody presses T.
    float: Option<Float>,

    /// How many times each slice's pixels have been written.
    ///
    /// **This is the layer list's invalidation rule, and it lives here because
    /// here is the only place a layer's pixels can change.** Every route — a
    /// stroke committing, a transform being put down, an undo writing a patch
    /// back, a layer cleared, a mask filled, a canvas flipped or resized — ends
    /// in one of this type's methods, so bumping a counter in each of them is
    /// exhaustive by construction. The alternative was a `touch` call beside
    /// every one of the eight call sites in `app.rs`, which is CLAUDE.md's
    /// "an invariant enforced at five call sites is one that will be forgotten
    /// at the sixth" written out in advance.
    ///
    /// Indexed by slot, and long enough for [`MAX_SLOTS`] from the start:
    /// growing it in step with the texture array would be a second place for
    /// the capacity to be got wrong. Two kilobytes — 256 `u64` is 2,048 bytes.
    /// This used to say "half a kilobyte", which was wrong by a factor of two
    /// at 129 slots as well; the figure is worth stating only because it is
    /// what makes allocating the whole thing up front obviously cheap.
    slot_revisions: Vec<u64>,
    /// The thumbnail being read back, if any. See [`ThumbJob`].
    thumb: Option<ThumbJob>,
    /// The thumbnail pass's target and staging buffer, allocated on the first
    /// request and reused for every one after it. Sixteen kilobytes each, so
    /// holding them is cheaper than the allocation churn of a per-job pair.
    thumb_target: Option<wgpu::Texture>,
    thumb_buffer: Option<wgpu::Buffer>,
}

impl Shared {
    fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("umber-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let grain_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("umber-grain-sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        // ---- dab pass -------------------------------------------------------
        let dab_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dab"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/dab.wgsl").into()),
        });
        let dab_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dab-bgl"),
            entries: &[
                uniform_entry(0),
                texture_entry(1),
                sampler_entry(2),
                texture_entry(3),
                sampler_entry(4),
                texture_entry(5),
                texture_entry(6),
            ],
        });

        let dab_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("dab-pl"),
            bind_group_layouts: &[Some(&dab_layout)],
            immediate_size: 0,
        });
        let dab_primitive = wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..Default::default()
        };

        // One descriptor, four pipelines. `colored` decides the fragment entry
        // point and whether the colour scratch is attached; `build_up` decides
        // only the coverage target's blend state, which is the whole reason it
        // is a pipeline choice and not a shader branch.
        let dab_pipelines = std::array::from_fn(|i| {
            let style = DabStyle {
                per_dab_color: i & 1 != 0,
                build_up: i & 2 != 0,
            };
            let coverage = if style.build_up {
                COVERAGE_BUILDUP_TARGET
            } else {
                COVERAGE_TARGET
            };
            let targets: &[Option<wgpu::ColorTargetState>] = if style.per_dab_color {
                &[Some(coverage), Some(STROKE_COLOR_TARGET)]
            } else {
                &[Some(coverage)]
            };
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(match i {
                    0 => "dab-pipeline",
                    1 => "dab-colored-pipeline",
                    2 => "dab-buildup-pipeline",
                    _ => "dab-colored-buildup-pipeline",
                }),
                layout: Some(&dab_pl),
                vertex: wgpu::VertexState {
                    module: &dab_shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: DAB_VERTEX_LAYOUT,
                },
                fragment: Some(wgpu::FragmentState {
                    module: &dab_shader,
                    entry_point: Some(if style.per_dab_color {
                        "fs_colored"
                    } else {
                        "fs"
                    }),
                    compilation_options: Default::default(),
                    targets,
                }),
                primitive: dab_primitive,
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        });

        // ---- composite pass -------------------------------------------------
        //
        // `blend.wgsl` in front, so the blend modes are compiled from one text
        // shared with the commit pass rather than written out twice. See that
        // file: two copies of Multiply is exactly the drift that makes a stroke
        // jump at pointer-up.
        let composite_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("composite"),
            source: wgpu::ShaderSource::Wgsl(BLEND_PRELUDE_COMPOSITE.into()),
        });
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("composite-bgl"),
            entries: &[
                uniform_entry(0),
                texture_array_entry(1),
                texture_entry(2),
                sampler_entry(3),
                texture_entry(4),
            ],
        });
        let composite_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("composite-pl"),
            bind_group_layouts: &[Some(&composite_layout)],
            immediate_size: 0,
        });
        // One composite *shader*, two pipelines, differing only in target
        // format. The blend maths — the thing that must never be duplicated —
        // is shared; what cannot be shared is the format a pipeline is compiled
        // against, and the window's is whatever the swapchain hands us.
        let make_composite = |label: &str, format: wgpu::TextureFormat| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&composite_pl),
                vertex: wgpu::VertexState {
                    module: &composite_shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &composite_shader,
                    entry_point: Some("fs"),
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
        let composite_pipeline = make_composite("composite-pipeline", surface_format);
        let composite_offscreen_pipeline =
            make_composite("composite-offscreen-pipeline", OFFSCREEN_FORMAT);

        // ---- commit pass ----------------------------------------------------
        //
        // `blend.wgsl` in front of this one too — see `BLEND_PRELUDE_COMMIT`.
        // The preview and the commit call one `composite_over` rather than each
        // carrying a copy of it.
        let commit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("commit"),
            source: wgpu::ShaderSource::Wgsl(BLEND_PRELUDE_COMMIT.into()),
        });
        let commit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("commit-bgl"),
            entries: &[
                uniform_entry(0),
                texture_entry(1),
                sampler_entry(2),
                texture_entry(3),
            ],
        });

        let commit_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("commit-pl"),
            bind_group_layouts: &[Some(&commit_layout)],
            immediate_size: 0,
        });

        // Paint and erase share a shader but need different blend state.
        //
        // Paint is ordinary premultiplied source-over. Erase cannot be: with
        // `src_factor: One` the alpha channel computes
        // `a = cov + dst.a * (1 - cov)`, which *adds* opacity — an eraser that
        // paints. Zeroing the source factor gives `a = dst.a * (1 - cov)`,
        // which is what removing coverage actually means.
        let erase_blend = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Zero,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        };
        let make_commit_pipeline = |label: &str, blend: wgpu::BlendState| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&commit_pl),
                vertex: wgpu::VertexState {
                    module: &commit_shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &commit_shader,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: LAYER_FORMAT,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };

        let commit_pipeline = make_commit_pipeline(
            "commit-paint",
            wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
        );
        let commit_erase_pipeline = make_commit_pipeline(
            "commit-erase",
            wgpu::BlendState {
                color: erase_blend,
                alpha: erase_blend,
            },
        );

        // The blended commit: everything the fixed-function blender cannot do.
        //
        // Multiply needs the pixel underneath, and no combination of blend
        // factors produces `B(Cb, Cs)`, so `fs_blend` computes the whole result
        // and the target's blend is `None`. The destination it needs is bound
        // at 4 as a copy, because a colour attachment may not also be sampled
        // — the same constraint `flip.wgsl` works around.
        //
        // The uniform carries a **dynamic offset**: one block per damaged
        // piece, because each piece is drawn against its own backdrop copy and
        // the vertex shader spans the piece rather than the whole rectangle.
        // One buffer and one bind group either way; the alternative was a bind
        // group per piece, which is allocation churn on pointer-up for nothing.
        let commit_blend_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("commit-blend-bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: true,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    texture_entry(1),
                    sampler_entry(2),
                    texture_entry(3),
                    texture_entry(4),
                ],
            });
        let commit_blend_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("commit-blend-pl"),
            bind_group_layouts: &[Some(&commit_blend_layout)],
            immediate_size: 0,
        });
        let commit_blend_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("commit-blend"),
                layout: Some(&commit_blend_pl),
                vertex: wgpu::VertexState {
                    module: &commit_shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &commit_shader,
                    entry_point: Some("fs_blend"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: LAYER_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });

        // ---- flip pass ------------------------------------------------------
        //
        // No sampler in the layout at all: `flip.wgsl` reads with
        // `textureLoad`, and a sampler nothing uses would be a suggestion that
        // it filters. The target is `LAYER_FORMAT_LINEAR` and the blend is
        // `None`, which together are what make the pass an exact permutation.
        let flip_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("flip"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/flip.wgsl").into()),
        });
        let flip_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("flip-bgl"),
            entries: &[uniform_entry(0), texture_entry(1)],
        });
        let flip_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("flip-pl"),
            bind_group_layouts: &[Some(&flip_layout)],
            immediate_size: 0,
        });
        let flip_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("flip"),
            layout: Some(&flip_pl),
            vertex: wgpu::VertexState {
                module: &flip_shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &flip_shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: LAYER_FORMAT_LINEAR,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // ---- thumbnail pass -------------------------------------------------
        //
        // No sampler, for the reason the flip pass has none: `thumbnail.wgsl`
        // reads with `textureLoad`, because a bilinear tap at a reduction of
        // 30:1 drops nearly every texel it is meant to be summarising.
        let thumb_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("thumbnail"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/thumbnail.wgsl").into()),
        });
        let thumb_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("thumb-bgl"),
            entries: &[uniform_entry(0), texture_array_entry(1)],
        });
        let thumb_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("thumb-pl"),
            bind_group_layouts: &[Some(&thumb_layout)],
            immediate_size: 0,
        });
        let thumb_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("thumbnail"),
            layout: Some(&thumb_pl),
            vertex: wgpu::VertexState {
                module: &thumb_shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &thumb_shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    // Non-sRGB, matching every other offscreen target here: the
                    // shader does its own encode and a typed target would do it
                    // twice.
                    format: OFFSCREEN_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // ---- transform pass -------------------------------------------------
        let transform_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("transform"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/transform.wgsl").into()),
        });
        let transform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("transform-bgl"),
            entries: &[
                uniform_entry(0),
                texture_entry(1),
                sampler_entry(2),
                texture_entry(3),
            ],
        });
        let transform_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("transform-pl"),
            bind_group_layouts: &[Some(&transform_layout)],
            immediate_size: 0,
        });

        // The two mask pipelines differ only in their blend state, and the
        // blend state is the whole of what they do: `fs_mask` writes coverage
        // into alpha and zero into colour, so with the source factor zeroed the
        // target is scaled by the mask or by its complement. Written as one
        // closure rather than two descriptors, for the reason the dab
        // pipelines are.
        let make_transform_pipeline = |label: &str, entry: &str, blend: wgpu::BlendState| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&transform_pl),
                vertex: wgpu::VertexState {
                    module: &transform_shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &transform_shader,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: LAYER_FORMAT,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let scale_by = |dst: wgpu::BlendFactor| {
            let c = wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Zero,
                dst_factor: dst,
                operation: wgpu::BlendOperation::Add,
            };
            wgpu::BlendState { color: c, alpha: c }
        };
        let transform_keep_pipeline = make_transform_pipeline(
            "transform-keep",
            "fs_mask",
            scale_by(wgpu::BlendFactor::SrcAlpha),
        );
        let transform_take_pipeline = make_transform_pipeline(
            "transform-take",
            "fs_mask",
            scale_by(wgpu::BlendFactor::OneMinusSrcAlpha),
        );
        let transform_draw_pipeline = make_transform_pipeline(
            "transform-draw",
            "fs_sample",
            wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
        );

        Self {
            sampler,
            grain_sampler,
            dab_pipelines,
            dab_layout,
            composite_pipeline,
            composite_offscreen_pipeline,
            composite_layout,
            commit_layout,
            commit_pipeline,
            commit_erase_pipeline,
            commit_blend_layout,
            commit_blend_pipeline,
            flip_layout,
            flip_pipeline,
            transform_layout,
            thumb_layout,
            thumb_pipeline,
            transform_keep_pipeline,
            transform_take_pipeline,
            transform_draw_pipeline,
        }
    }
}

impl CanvasRenderer {
    pub fn new(
        device: &wgpu::Device,
        doc_size: UVec2,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        Self::with_shared(device, doc_size, Shared::new(device, surface_format))
    }

    /// A renderer for a second document, reusing this one's compiled pipelines.
    ///
    /// Layer storage is emphatically *not* shared: each document owns its own
    /// texture array and its own stroke scratch, so switching tabs is a
    /// different renderer rather than a reallocation and a re-upload. What is
    /// shared is everything that would otherwise be recompiled — see [`Shared`].
    ///
    /// The new renderer's textures hold whatever the allocation contained, so
    /// the caller must clear them before the first composite, exactly as it
    /// does after [`CanvasRenderer::new`] — and set the new document's
    /// background, which is its own and not this one's.
    pub fn for_document(&self, device: &wgpu::Device, doc_size: UVec2) -> Self {
        Self::with_shared(device, doc_size, self.shared.clone())
    }

    fn with_shared(device: &wgpu::Device, doc_size: UVec2, shared: Shared) -> Self {
        let layers = LayerStore::new(device, doc_size, INITIAL_SLOTS);

        let stroke = make_stroke_texture(device, doc_size);
        let stroke_view = stroke.create_view(&wgpu::TextureViewDescriptor::default());

        // A 1x1 stand-in, exactly as the tip has. Nearly every stroke paints one
        // colour and never touches this, so a document-sized allocation here
        // would be megabytes held for a feature most sessions never use.
        let stroke_color = make_stroke_color_texture(device, UVec2::ONE);
        let stroke_color_view = stroke_color.create_view(&wgpu::TextureViewDescriptor::default());

        let dab_uniforms = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("dab-uniforms"),
            contents: bytemuck::bytes_of(&DabUniforms::plain(doc_size)),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let dab_instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dab-instances"),
            size: DAB_STRIDE * MAX_DABS_PER_FRAME as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // A 1x1 placeholder stands in when no tip is set, so the bind group
        // layout never varies and there is still exactly one dab pipeline. Its
        // contents do not matter: with `use_tip` at zero the shader samples it
        // and discards the result, which is the price of keeping
        // `textureSample` out of non-uniform control flow.
        let tip = make_tip_texture(device, 1, 1);
        let tip_view = tip.create_view(&wgpu::TextureViewDescriptor::default());
        // The same placeholder trick for the paper. Its contents do not matter
        // either: with `grain_strength` at zero the shader's `mix` returns
        // exactly 1.0 whatever was sampled.
        let grain = make_tip_texture(device, 1, 1);
        let grain_view = grain.create_view(&wgpu::TextureViewDescriptor::default());
        // A placeholder again, but for a different reason: this one is never
        // sampled, because `use_selection` is zero and the shader's `select`
        // returns 1.0 without looking. It exists so the bind group layout does
        // not vary — there is still exactly one set of dab pipelines.
        let selection = make_coverage_texture(device, 1, 1, "umber-selection-mask");
        let selection_view = selection.create_view(&wgpu::TextureViewDescriptor::default());
        // The tip's colour, when it has one. A placeholder for the tip's own
        // reason rather than the selection's: `use_tip_color` is zero, so
        // `fs_colored` samples it and throws the answer away, and `fs` — every
        // ordinary stroke — does not sample it at all.
        let tip_color = make_tip_color_texture(device, 1, 1);
        let tip_color_view = tip_color.create_view(&wgpu::TextureViewDescriptor::default());
        let dab_bind_group = make_dab_bind_group(
            device,
            &shared.dab_layout,
            &dab_uniforms,
            &tip_view,
            &shared.sampler,
            &grain_view,
            &shared.grain_sampler,
            &selection_view,
            &tip_color_view,
        );

        let view_uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("view-uniforms"),
            size: std::mem::size_of::<ViewUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let composite_bind_group = make_composite_bind_group(
            device,
            &shared.composite_layout,
            &view_uniforms,
            &layers.array_view,
            &stroke_view,
            &shared.sampler,
            &stroke_color_view,
        );

        let commit_uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("commit-uniforms"),
            size: std::mem::size_of::<CommitUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let commit_bind_group = make_commit_bind_group(
            device,
            &shared.commit_layout,
            &commit_uniforms,
            &stroke_view,
            &shared.sampler,
            &stroke_color_view,
        );

        Self {
            doc_size,
            // Transparent until the caller says otherwise, which is what the
            // canvas looked like before documents had a background at all.
            background: Background::Transparent.premultiplied(),
            shared,
            layers,
            stroke,
            stroke_view,
            stroke_color,
            stroke_color_view,
            has_stroke_color: false,
            probes: (0..PROBE_SLOTS)
                .map(|i| Probe {
                    buffer: device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some(&format!("umber-probe-{i}")),
                        size: (PROBE_ROW_BYTES * PROBE_SIZE) as u64,
                        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    }),
                    state: ProbeState::Idle,
                    outcome: Arc::new(AtomicU8::new(PROBE_PENDING)),
                    stale: false,
                })
                .collect(),
            capture: None,
            readback_limit: device.limits().max_buffer_size,
            dab_bind_group,
            dab_uniforms,
            dab_instances,
            dabs_this_frame: 0,
            tip,
            has_tip: false,
            tip_color,
            tip_color_view,
            tip_mask: None,
            grain,
            grain_view,
            grain_tile: None,
            selection,
            selection_view,
            selection_mask: None,
            grain_params: (0.0, 1.0),
            dab_state: DabUniforms::plain(doc_size),
            composite_bind_group,
            view_uniforms,
            commit_bind_group,
            commit_uniforms,
            float: None,
            slot_revisions: vec![0; MAX_SLOTS],
            thumb: None,
            thumb_target: None,
            thumb_buffer: None,
        }
    }

    pub fn doc_size(&self) -> UVec2 {
        self.doc_size
    }

    /// Set what lies under this document's layer stack.
    ///
    /// Costs a field write: the value reaches the GPU with the rest of the view
    /// uniforms on the next composite, so changing it mid-frame is free and
    /// dragging a colour picker over it does not touch a buffer.
    pub fn set_background(&mut self, background: Background) {
        self.background = background.premultiplied();
    }

    pub fn slot_capacity(&self) -> u32 {
        self.layers.capacity
    }

    /// Guarantee at least `needed` texture-array slices exist.
    ///
    /// Growth reallocates the array and copies every existing slice, so it
    /// doubles rather than growing by one — a document that reaches eight
    /// layers pays for two copies, not eight. **Only while the array is cheap**:
    /// past a byte budget it grows to exactly what was asked for, because a
    /// slice is canvas-sized and doubling one is not an optimisation but a
    /// gigabyte. [`grown_capacity`] is the whole policy and has the argument.
    ///
    /// **The `.min` below fails open and the assertion is what stops it.**
    /// Asked for more than [`MAX_SLOTS`], this allocates the ceiling, logs a
    /// growth line naming it, and returns as though it had done what it was
    /// asked — after which every slot at or above the ceiling indexes off the
    /// end of the array. It is unreachable today, because `SlotPool` hands out
    /// at most slot 255 and `begin_float` refuses at the ceiling; but this
    /// clamp is the *only* thing standing between those two guarantees and
    /// silently wrong pixels, and `LayerStack::MAX_SLOTS` lives in a different
    /// crate, where it can be raised on its own with only one test in this one
    /// to notice. A named failure is worth the debug build's comparison.
    pub fn ensure_slots(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, needed: u32) {
        debug_assert!(
            needed <= MAX_SLOTS as u32,
            "asked for {needed} slices against a ceiling of {MAX_SLOTS}"
        );
        if needed <= self.layers.capacity {
            return;
        }
        let slice_bytes =
            u64::from(self.doc_size.x) * u64::from(self.doc_size.y) * LAYER_BYTES_PER_PIXEL;
        let capacity =
            grown_capacity(self.layers.capacity, needed, slice_bytes).min(MAX_SLOTS as u32);
        log::info!(
            "growing layer storage {} -> {} slots",
            self.layers.capacity,
            capacity
        );

        let grown = LayerStore::new(device, self.doc_size, capacity);

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("grow-layers"),
        });
        enc.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.layers.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &grown.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: self.doc_size.x,
                height: self.doc_size.y,
                depth_or_array_layers: self.layers.capacity,
            },
        );
        // Slices beyond the old capacity are freshly allocated and hold
        // whatever the driver left behind.
        for slot in self.layers.capacity..capacity {
            clear_view(&mut enc, &grown.slot_views[slot as usize], "clear-new-slot");
        }
        queue.submit(Some(enc.finish()));

        self.composite_bind_group = make_composite_bind_group(
            device,
            &self.shared.composite_layout,
            &self.view_uniforms,
            &grown.array_view,
            &self.stroke_view,
            &self.shared.sampler,
            &self.stroke_color_view,
        );
        self.layers = grown;
    }

    /// Change the canvas size, carrying the artwork across.
    ///
    /// Every one of this document's textures is sized to the canvas, so a
    /// resize reallocates all of them and copies the surviving rectangle into
    /// the new ones. Where that rectangle lands is [`CanvasCopy::plan`]'s to
    /// decide, in `umber-core`, so the app's preview of a resize and what the
    /// GPU actually does cannot drift apart.
    ///
    /// The layer array is copied **whole**, all slices in one transfer: the
    /// anchor moves the picture, not one layer relative to another, so the
    /// origin is the same for every slice and the depth of the copy is the slot
    /// capacity. The new array is cleared first, because the region outside the
    /// surviving rectangle is freshly allocated memory.
    ///
    /// Two things the caller owes this:
    ///
    /// * **No stroke in flight.** The scratch is thrown away rather than
    ///   resampled — a half-painted stroke has no meaning at a new size, and
    ///   rescaling coverage would soften the mark it is about to commit.
    /// * **Clear the undo history.** Every `PixelPatch` is a rectangle in the
    ///   *old* geometry; replaying one after a resize would paste the right
    ///   bytes into the wrong pixels, or name a rectangle off the edge. This is
    ///   the same reason deleting a layer clears it, and structural undo is the
    ///   same real fix.
    /// * **Drop the selection**, for the same reason again: its bounds are a
    ///   rectangle of the old canvas and can now name pixels that do not exist.
    ///   This drops the *mask*, so a caller that forgot leaves a document that
    ///   is unclipped rather than one clipped to the wrong place — but it also
    ///   leaves the outline on screen describing nothing, so do not forget.
    pub fn resize(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        new_size: UVec2,
        anchor: Anchor,
    ) {
        let new_size = new_size.max(UVec2::ONE);
        if new_size == self.doc_size {
            return;
        }
        // Every thumbnail is a picture of a canvas that is about to stop
        // existing, and the one in flight would come home describing the old
        // geometry through the new document's arithmetic.
        self.touch_all_slots();
        let plan = CanvasCopy::plan(self.doc_size, new_size, anchor);
        log::info!(
            "resizing canvas {} x {} -> {} x {}, {anchor:?}",
            self.doc_size.x,
            self.doc_size.y,
            new_size.x,
            new_size.y,
        );

        let resized = LayerStore::new(device, new_size, self.layers.capacity);
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("resize-canvas"),
        });
        for view in &resized.slot_views {
            clear_view(&mut enc, view, "clear-resized-slot");
        }
        enc.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.layers.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: plan.from.x,
                    y: plan.from.y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &resized.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: plan.to.x,
                    y: plan.to.y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: plan.size.x,
                height: plan.size.y,
                depth_or_array_layers: self.layers.capacity,
            },
        );
        queue.submit(Some(enc.finish()));
        self.layers = resized;

        // The scratch is the stroke in progress, and there is not one — see the
        // contract above. Reallocated rather than copied, and it starts clear
        // like any freshly allocated target.
        self.stroke = make_stroke_texture(device, new_size);
        self.stroke_view = self
            .stroke
            .create_view(&wgpu::TextureViewDescriptor::default());
        if self.has_stroke_color {
            self.stroke_color = make_stroke_color_texture(device, new_size);
            self.stroke_color_view = self
                .stroke_color
                .create_view(&wgpu::TextureViewDescriptor::default());
        }
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("clear-resized-scratch"),
        });
        self.clear_stroke(&mut enc);
        queue.submit(Some(enc.finish()));

        // Its base and its floating copy are canvas-sized and its rectangles
        // name pixels that no longer exist. Thrown away rather than resampled,
        // for the reason the scratch is: a half-finished gesture has no meaning
        // at a new size, and the caller owes this no stroke and no float in
        // flight anyway.
        self.end_float();
        // A sample recorded against the old canvas would be read back as if it
        // belonged to the new one.
        self.reset_probes();
        // And a capture half-read against the old canvas would be assembled
        // into a file with layers of two different sizes in it.
        self.cancel_capture();

        self.doc_size = new_size;
        self.dab_state.doc_size = [new_size.x as f32, new_size.y as f32];
        // The mask names pixels of a canvas that no longer exists. Dropped
        // rather than rescaled: a selection is the artist's statement about
        // where they are working, and a resampled one is a guess.
        // `set_selection` rebuilds the bind group, so this must run before the
        // uniform write rather than after it.
        self.set_selection(device, queue, None);
        queue.write_buffer(&self.dab_uniforms, 0, bytemuck::bytes_of(&self.dab_state));

        self.composite_bind_group = make_composite_bind_group(
            device,
            &self.shared.composite_layout,
            &self.view_uniforms,
            &self.layers.array_view,
            &self.stroke_view,
            &self.shared.sampler,
            &self.stroke_color_view,
        );
        self.commit_bind_group = make_commit_bind_group(
            device,
            &self.shared.commit_layout,
            &self.commit_uniforms,
            &self.stroke_view,
            &self.shared.sampler,
            &self.stroke_color_view,
        );
    }

    /// Set the bitmap tip the dab pass stamps, or `None` for the procedural
    /// round brush.
    ///
    /// The tip is bound for the whole dab pass rather than carried per dab, so
    /// a thousand tipped dabs are still a single draw call. Change it *between*
    /// strokes: a stroke has one brush, and swapping mid-pass would restamp the
    /// dabs already in the scratch under the new shape.
    ///
    /// What the tip does is modulate coverage. It is not composited and it does
    /// not touch the blend state, so overlapping tipped dabs still saturate at
    /// 1.0 and stroke opacity is still applied once, at commit —
    /// `a_tipped_stamp_still_saturates_under_overlap` guards that.
    ///
    /// Called at the start of every stroke, so it early-outs when the mask has
    /// not changed. The test is `Arc` **identity**, not equality: masks are
    /// shared out of the brush library, so two brushes cut from one stamp
    /// really are one allocation, and comparing a megabyte of coverage to
    /// answer "same brush as last time?" would put the cost back. Without the
    /// guard a texture allocation and a copy land on the first frame of every
    /// stroke, which is the one moment this project exists to keep short.
    ///
    /// `stamps_color` is whether a **coloured stamp**'s colour should be
    /// honoured, and it is a parameter rather than being read off the mask on
    /// purpose. It has to be the *same decision* as
    /// [`StrokeStyle::per_dab_color`], which turns on for a smudging brush as
    /// well — so a brush that smudges *and* carries a coloured tip would
    /// otherwise take the coloured pipeline for one reason and stamp its tip's
    /// colour for another, in cases where the caller had refused the second.
    /// An eraser and a stroke on a mask are exactly those cases: neither has
    /// anywhere for a colour to land, a mask is read on `.r`, and the result
    /// was a mask stroke that previewed grey and committed the stamp's red.
    /// One argument, decided once by the caller, is what stops the two.
    pub fn set_tip(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tip: Option<Arc<TipMask>>,
        stamps_color: bool,
    ) {
        // What the colour plane is about to be. Part of the early-out, because
        // the same brush can come back with the answer changed — pick up the
        // eraser without changing tip and the stamp must stop colouring.
        let want_color = stamps_color && tip.as_ref().is_some_and(|mask| mask.is_coloured());
        let same_mask = match (&self.tip_mask, &tip) {
            (Some(current), Some(next)) => Arc::ptr_eq(current, next),
            (None, None) => true,
            _ => false,
        };
        if same_mask && self.dab_state.use_tip_color == u32::from(want_color) {
            return;
        }

        let (texture, has_tip) = match &tip {
            Some(mask) => {
                // The mask's own proportions. Padding it into a square would
                // reach the same geometry and pay for an empty margin in
                // texture memory and in fragments — see `TipMask::aspect`.
                let (sx, sy) = mask.aspect();
                self.dab_state.tip_scale = [sx, sy];
                self.dab_state.use_tip = 1;
                (upload_mask(device, queue, mask), true)
            }
            None => {
                self.dab_state.tip_scale = [1.0, 1.0];
                self.dab_state.use_tip = 0;
                (make_tip_texture(device, 1, 1), false)
            }
        };

        // A coloured stamp's second plane. Uploaded here rather than lazily,
        // because it is bound for the whole dab pass exactly as the coverage is
        // — and dropped back to the placeholder the moment a tip without one is
        // chosen, or the moment the caller declines it, so a session that
        // touched one coloured brush does not go on holding its pixels.
        let color = want_color
            .then_some(tip.as_ref())
            .flatten()
            .and_then(|mask| mask.colour_premultiplied().map(|rgba| (mask, rgba)));
        let color_texture = match color {
            Some((mask, rgba)) => upload_tip_color(device, queue, mask, &rgba),
            None => make_tip_color_texture(device, 1, 1),
        };
        self.dab_state.use_tip_color = u32::from(want_color);
        self.tip_color_view = color_texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.tip_color = color_texture;

        self.tip = texture;
        self.has_tip = has_tip;
        self.tip_mask = tip;
        self.rebuild_dab_bind_group(device);
        queue.write_buffer(&self.dab_uniforms, 0, bytemuck::bytes_of(&self.dab_state));
    }

    /// Whether the dab pass will stamp the tip's own colour.
    ///
    /// This is [`Self::set_tip`]'s `stamps_color` **and** the tip actually
    /// carrying a colour, which is the one thing that has to agree with
    /// [`StrokeStyle::per_dab_color`]: a stroke that stamped a colour without
    /// the colour attachment attached would have it thrown away and commit as
    /// the flat palette colour. The caller decides both from one snapshot — see
    /// `Editor::begin_stroke` — so this reports rather than requires.
    pub fn stamps_tip_color(&self) -> bool {
        self.dab_state.use_tip_color != 0
    }

    /// Set the paper the dab pass bites through, or `None` for none.
    ///
    /// Per stroke, exactly as [`Self::set_tip`] is and for the same reasons: one
    /// binding covers a whole dab pass, and changing it mid-stroke would leave
    /// the dabs already in the scratch textured by the previous paper.
    ///
    /// The tile is compared by `Arc` identity, so calling this every stroke with
    /// the same paper costs a pointer comparison. `strength` and `scale` are
    /// compared by value and cost a uniform write when they change — no texture
    /// upload and no bind group, which is what makes dragging the Texture
    /// section's sliders cheap.
    ///
    /// A strength of zero is the **exact identity**: the shader computes
    /// `mix(1.0, tile, strength)`, which at zero is 1.0 whatever the tile holds.
    /// `grain_off_is_the_exact_identity` is the guard, and it is why an ordinary
    /// brush pays one multiply rather than a branch.
    pub fn set_grain(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        grain: Option<(Arc<TipMask>, f32, f32)>,
    ) {
        let (tile, strength, scale) = match grain {
            Some((tile, strength, scale)) => (Some(tile), strength.clamp(0.0, 1.0), scale.max(1.0)),
            // Nothing to bind and nothing to sample: leave whatever tile is
            // already uploaded where it is and turn the strength off. A painter
            // who reaches for grain once will reach for it again, and dropping
            // the texture would mean re-uploading it on the next stroke.
            None => (self.grain_tile.clone(), 0.0, self.dab_state.grain_scale),
        };

        let same_tile = match (&self.grain_tile, &tile) {
            (Some(current), Some(next)) => Arc::ptr_eq(current, next),
            (None, None) => true,
            _ => false,
        };
        if same_tile && self.grain_params == (strength, scale) {
            return;
        }

        if !same_tile {
            let texture = match &tile {
                Some(mask) => upload_mask(device, queue, mask),
                None => make_tip_texture(device, 1, 1),
            };
            self.grain_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.grain = texture;
            self.grain_tile = tile;
            self.rebuild_dab_bind_group(device);
        }

        self.grain_params = (strength, scale);
        self.dab_state.grain_strength = strength;
        self.dab_state.grain_scale = scale;
        queue.write_buffer(&self.dab_uniforms, 0, bytemuck::bytes_of(&self.dab_state));
    }

    /// Clip the dab pass to `selection`, or to nothing at all with `None`.
    ///
    /// Per stroke, exactly as [`Self::set_tip`] and [`Self::set_grain`] are, and
    /// for the same reason: one binding covers a whole dab pass, so changing it
    /// mid-stroke would leave the coverage already in the scratch clipped by the
    /// selection that has gone. The mask is compared by `Arc` identity, so
    /// calling this at the start of every stroke costs a pointer comparison.
    ///
    /// **The selection is applied here and nowhere else.** The scratch then
    /// holds coverage that is already clipped, so the preview and the commit —
    /// which must implement identical blending maths — cannot disagree about
    /// where the selection was, because neither of them knows there is one.
    pub fn set_selection(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        selection: Option<Arc<Selection>>,
    ) {
        let unchanged = match (&self.selection_mask, &selection) {
            (Some(current), Some(next)) => Arc::ptr_eq(current, next),
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return;
        }

        let texture = match &selection {
            Some(sel) => {
                let rect = sel.bounds();
                self.dab_state.sel_min = [rect.x as f32, rect.y as f32];
                self.dab_state.sel_size = [rect.width as f32, rect.height as f32];
                self.dab_state.use_selection = 1;
                upload_coverage(
                    device,
                    queue,
                    rect.width,
                    rect.height,
                    sel.coverage(),
                    "umber-selection-mask",
                )
            }
            None => {
                self.dab_state.sel_min = [0.0, 0.0];
                // Not zero: the shader divides by this, and a NaN would take
                // the whole dab with it rather than merely being discarded.
                self.dab_state.sel_size = [1.0, 1.0];
                self.dab_state.use_selection = 0;
                make_coverage_texture(device, 1, 1, "umber-selection-mask")
            }
        };

        self.selection_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.selection = texture;
        self.selection_mask = selection;
        self.rebuild_dab_bind_group(device);
        queue.write_buffer(&self.dab_uniforms, 0, bytemuck::bytes_of(&self.dab_state));
    }

    fn rebuild_dab_bind_group(&mut self, device: &wgpu::Device) {
        let tip_view = self
            .tip
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.dab_bind_group = make_dab_bind_group(
            device,
            &self.shared.dab_layout,
            &self.dab_uniforms,
            &tip_view,
            &self.shared.sampler,
            &self.grain_view,
            &self.shared.grain_sampler,
            &self.selection_view,
            &self.tip_color_view,
        );
    }

    /// Whether a bitmap tip is currently bound.
    pub fn has_tip(&self) -> bool {
        self.has_tip
    }

    /// Reset the per-frame instance cursor. Call once at the top of a frame.
    pub fn begin_frame(&mut self) {
        self.dabs_this_frame = 0;
    }

    /// Give the stroke somewhere to record a colour per dab.
    ///
    /// Allocated the first time a smudging stroke needs it and kept thereafter:
    /// a painter who reaches for a blender once will reach for it again, and
    /// re-allocating a document-sized texture per stroke would be a stutter at
    /// exactly the wrong moment. The two bind groups that name it have to be
    /// rebuilt, which is why this is not simply a lazy getter.
    fn ensure_stroke_color(&mut self, device: &wgpu::Device) {
        if self.has_stroke_color {
            return;
        }
        self.stroke_color = make_stroke_color_texture(device, self.doc_size);
        self.stroke_color_view = self
            .stroke_color
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.has_stroke_color = true;

        self.composite_bind_group = make_composite_bind_group(
            device,
            &self.shared.composite_layout,
            &self.view_uniforms,
            &self.layers.array_view,
            &self.stroke_view,
            &self.shared.sampler,
            &self.stroke_color_view,
        );
        self.commit_bind_group = make_commit_bind_group(
            device,
            &self.shared.commit_layout,
            &self.commit_uniforms,
            &self.stroke_view,
            &self.shared.sampler,
            &self.stroke_color_view,
        );
    }

    /// Upload dabs and stamp them into the scratch texture.
    ///
    /// `style` must be the **same for every frame of a stroke**, and its
    /// `per_dab_color` must match the [`StrokeStyle`] handed to
    /// [`Self::composite`] and [`Self::commit_stroke`]. Turning colour on midway
    /// would leave the earlier dabs with no colour recorded, and they would
    /// commit as the flat palette colour while the rest smudged; turning
    /// build-up on midway would leave the first half of the stroke saturating
    /// where the second half accumulates, which is a visible step in a mark that
    /// should be even.
    pub fn draw_dabs(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        dabs: &[Dab],
        style: DabStyle,
    ) {
        if dabs.is_empty() {
            return;
        }
        let colored = style.per_dab_color;
        if colored {
            self.ensure_stroke_color(device);
        }
        let room = MAX_DABS_PER_FRAME.saturating_sub(self.dabs_this_frame as usize);
        if room == 0 {
            log::warn!("dab instance buffer full, dropping {} dabs", dabs.len());
            return;
        }
        let dabs = &dabs[..dabs.len().min(room)];

        let offset = self.dabs_this_frame as u64 * DAB_STRIDE;
        queue.write_buffer(&self.dab_instances, offset, bytemuck::cast_slice(dabs));

        // Load, never clear: the scratch accumulates across frames for the whole
        // stroke, so only the new dabs are drawn each frame.
        let load = wgpu::Operations {
            load: wgpu::LoadOp::Load,
            store: wgpu::StoreOp::Store,
        };
        let coverage_attachment = Some(wgpu::RenderPassColorAttachment {
            view: &self.stroke_view,
            resolve_target: None,
            depth_slice: None,
            ops: load,
        });
        let color_attachment = Some(wgpu::RenderPassColorAttachment {
            view: &self.stroke_color_view,
            resolve_target: None,
            depth_slice: None,
            ops: load,
        });
        let attachments: &[Option<wgpu::RenderPassColorAttachment<'_>>] = if colored {
            &[coverage_attachment, color_attachment]
        } else {
            std::slice::from_ref(&coverage_attachment)
        };

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("dab-pass"),
            color_attachments: attachments,
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.shared.dab_pipelines[style.index()]);
        pass.set_bind_group(0, &self.dab_bind_group, &[]);
        pass.set_vertex_buffer(
            0,
            self.dab_instances
                .slice(offset..offset + dabs.len() as u64 * DAB_STRIDE),
        );
        pass.draw(0..4, 0..dabs.len() as u32);
        drop(pass);

        self.dabs_this_frame += dabs.len() as u32;
    }

    /// Draw the whole layer stack plus the in-progress stroke to `target`.
    pub fn composite(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        params: &CompositeParams<'_>,
    ) {
        let scale = 1.0 / params.camera.zoom;
        // Solving `doc = screen * scale + offset` for the pivot mapping to the
        // camera centre. Must stay in step with `Camera::screen_to_doc`, which
        // the input path uses — if they disagree, strokes land off the cursor.
        let offset = params.camera.center - params.pivot * scale;

        // Against [`MAX_DRAWS`], not [`MAX_LAYERS`]: `params.layers` is the
        // *draw* list the app flattened folders out of, which a layer's effects
        // will each add an entry to.
        let mut packed = [[0.0f32; 4]; MAX_DRAWS];
        let mut extra = [[0.0f32; 4]; MAX_DRAWS];
        let count = params.layers.len().min(MAX_DRAWS);
        for ((dst, ext), src) in packed
            .iter_mut()
            .zip(extra.iter_mut())
            .zip(&params.layers[..count])
        {
            *dst = [
                src.opacity.clamp(0.0, 1.0),
                src.blend as f32,
                src.slot as f32,
                if src.visible { 1.0 } else { 0.0 },
            ];
            *ext = [
                // The layer's own slice where there is no mask, so the array
                // index the shader samples is always in range and the result
                // is discarded by the flag beside it rather than by a branch.
                src.mask.unwrap_or(src.slot) as f32,
                if src.mask.is_some() { 1.0 } else { 0.0 },
                if src.clipped { 1.0 } else { 0.0 },
                0.0,
            ];
        }

        let color = params.stroke.color;
        queue.write_buffer(
            &self.view_uniforms,
            0,
            bytemuck::bytes_of(&ViewUniforms {
                scale: [scale, scale],
                offset: [offset.x, offset.y],
                doc_size: [self.doc_size.x as f32, self.doc_size.y as f32],
                pivot: [params.pivot.x, params.pivot.y],
                stroke_color: [
                    color.r,
                    color.g,
                    color.b,
                    params.stroke.opacity.clamp(0.0, 1.0),
                ],
                backdrop: [
                    params.backdrop[0],
                    params.backdrop[1],
                    params.backdrop[2],
                    1.0,
                ],
                background: self.background,
                layer_count: count as u32,
                stroke_mode: mode_index(params.stroke.mode),
                active_index: params.active_index,
                checker: 8.0,
                is_export: if params.export { 1 } else { 0 },
                per_dab_color: u32::from(params.stroke.per_dab_color),
                stroke_on_mask: u32::from(params.stroke.on_mask),
                // The brush's mode, and it reaches the shader for the paint
                // path alone — an eraser's branch never reads it. Not clamped
                // or coerced here: `Brush::blend_applies` is the one place that
                // decision is made, and restating it would be a second place
                // for the preview and the commit to disagree.
                stroke_blend: params.stroke.blend.index(),
                layers: packed,
                extra,
            }),
        );

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("composite-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        // `export` means both "straight alpha, no checkerboard" and "this is
        // not the window", and the two always travel together: every offscreen
        // target Umber composites into is `OFFSCREEN_FORMAT`.
        pass.set_pipeline(if params.export {
            &self.shared.composite_offscreen_pipeline
        } else {
            &self.shared.composite_pipeline
        });
        pass.set_bind_group(0, &self.composite_bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Bake the scratch stroke into `slot` over `rect`, then clear the scratch.
    /// Bake the finished stroke into the layer.
    ///
    /// `pieces` are the parts of `rect` the stroke actually reached, and the
    /// pass is scissored to them: **exactly the pixels the undo patch was
    /// captured from, and no others.** That equality is the whole of why a
    /// patch may be smaller than the stroke's bounding box. Committing the
    /// whole box instead would run every pixel of it through the blend — an
    /// identity where coverage is zero, but an identity computed in floating
    /// point and written back through an sRGB encode, which is a guarantee
    /// about rounding rather than a guarantee about pixels. Scissoring makes it
    /// a guarantee about pixels: an untouched cell is never written at all.
    ///
    /// It is also less work. A thin diagonal across a large canvas commits a
    /// hundred and fifty narrow strips instead of the whole document.
    ///
    /// A brush carrying a blend mode other than [`BlendMode::Normal`] goes down
    /// [`Self::commit_blended`] instead. Normal — every stroke there has ever
    /// been — is untouched: the same one pass, the same fixed-function blender,
    /// no copy and no allocation.
    ///
    /// The device is here because the blended path allocates — a backdrop
    /// texture, a uniform block per piece and a bind group, all dropped when it
    /// returns. Bundling the arguments to please the lint would hide which of
    /// them the two paths share; `rect` and `pieces` are separately meaningful
    /// (the first is what the quad spans, the second what survives the scissor)
    /// and the style is already the one struct preview and commit share.
    #[allow(clippy::too_many_arguments)]
    pub fn commit_stroke(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        slot: u32,
        rect: PixelRect,
        pieces: &[PixelRect],
        style: StrokeStyle,
    ) {
        let Some(view) = self.layers.slot_views.get(slot as usize) else {
            log::error!("commit to slot {slot} beyond capacity");
            return;
        };

        // Two strokes carry no blend, and both are *ignored* rather than
        // refused — the same reading `umber_core::Brush::blend_applies` gives,
        // and the editor never sends one here in either case.
        //
        // An eraser has no colour, so it has nothing to blend with what is
        // under it. A stroke on a mask has no colour either: the slice holds
        // coverage on one channel, and `fs_blend` writes four, so a blended
        // commit onto one would put colour into a mask. Guarding only the
        // eraser is the asymmetry that gets forgotten — a caller reaching
        // `commit_stroke` directly is all that stands between the two.
        let blends = style.mode == BrushMode::Paint && !style.on_mask;
        if blends && style.blend != BlendMode::Normal {
            self.commit_blended(device, encoder, slot, pieces, style);
            self.clear_stroke(encoder);
            self.touch_slot(slot);
            return;
        }

        let color = style.color;
        queue.write_buffer(
            &self.commit_uniforms,
            0,
            bytemuck::bytes_of(&CommitUniforms {
                rect_min: [rect.x as f32, rect.y as f32],
                rect_max: [(rect.x + rect.width) as f32, (rect.y + rect.height) as f32],
                doc_size: [self.doc_size.x as f32, self.doc_size.y as f32],
                _pad0: [0.0; 2],
                color: [color.r, color.g, color.b, style.opacity.clamp(0.0, 1.0)],
                mode: mode_index(style.mode),
                per_dab_color: u32::from(style.per_dab_color),
                blend: BlendMode::Normal.index(),
                _pad2: 0,
            }),
        );

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("commit-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(match style.mode {
                BrushMode::Paint => &self.shared.commit_pipeline,
                BrushMode::Erase => &self.shared.commit_erase_pipeline,
            });
            pass.set_bind_group(0, &self.commit_bind_group, &[]);
            // One quad, drawn once per piece under a different scissor. The
            // vertex shader spans `rect` every time — the scissor is what
            // decides which of it survives — so nothing per piece has to reach
            // the uniform buffer.
            for piece in pieces {
                pass.set_scissor_rect(piece.x, piece.y, piece.width, piece.height);
                pass.draw(0..4, 0..1);
            }
        }

        self.clear_stroke(encoder);
        self.touch_slot(slot);
    }

    /// The commit for a brush whose blend mode is not Normal.
    ///
    /// # Why this needs a copy at all
    ///
    /// Multiply is a function of the pixel underneath, and no combination of
    /// fixed-function blend factors can produce one — `B(Cb, Cs)` is not linear
    /// in the destination for Overlay, and even Multiply's premultiplied form
    /// needs the source twice. So the destination has to arrive as a *sampled*
    /// input, and a colour attachment may not also be bound for sampling. The
    /// pass therefore reads a copy, which is the arrangement `flip.wgsl` uses
    /// for the same reason.
    ///
    /// # Why the copy is per piece
    ///
    /// A backdrop covering the whole damaged rectangle would be canvas-sized
    /// for a stroke drawn across the picture, and — much worse — it would be
    /// canvas-sized for a *thin diagonal* too, since that stroke's bounding box
    /// is the whole document. That is the 381 MB the tiled undo patch exists to
    /// avoid, put back on the GPU. A `TileMask` piece is a contiguous *run* of
    /// cells within one row of the 64-pixel damage grid — `push_run` emits one
    /// per run, so a row may hold several — and each is therefore never taller
    /// than a cell nor wider than the stroke's own rectangle. That is what
    /// bounds the copy at `canvas width × 64` however long the stroke is; the
    /// texture is sized to the largest single piece.
    ///
    /// A caller that hands over the bounding rectangle as one piece gets a
    /// backdrop the size of it, and that is the honest bound rather than a
    /// hole in the one above: the undo patch for that same commit is the whole
    /// rectangle too, so the backdrop is never larger than what the caller was
    /// already paying for on the CPU.
    ///
    /// The cost is a render pass per piece rather than one pass with a scissor
    /// per piece, because a copy cannot be recorded inside a pass. Since a piece
    /// is a run rather than a row, that count follows how much the stroke
    /// zig-zags and not only how long it is — a hundred and fifty passes for a
    /// thin diagonal across the largest canvas, and several times that for a
    /// stroke that crosses its own row repeatedly. Once, at pointer-up, on a
    /// path that already does a blocking readback for the undo patch, and only
    /// for a brush that asked for a blend mode.
    ///
    /// That argument forbids *interleaving* copies and passes; it does not
    /// forbid recording every copy first and then drawing one pass. Copying the
    /// pieces into a single atlas and drawing them under the scissor and dynamic
    /// offset that already exist would be one pass, and is the change to make if
    /// this ever needs to be cheaper — batched to a byte budget, because the
    /// total piece area is 6.8 MB for that diagonal and 381 MB for a wash that
    /// covers the canvas. It is not worth it on the desktop, where the scissor
    /// makes an extra pass nearly free. It would matter on a tile-based
    /// renderer, where wgpu's render area is the whole attachment and each pass
    /// loads and stores every tile of the slice — which is Android and iOS, and
    /// neither has ever been built.
    ///
    /// Everything allocated here is dropped when the commit returns. A stroke
    /// is not often enough to be worth caching, and caching would mean holding
    /// a canvas-wide texture for a session because one stroke wanted it.
    fn commit_blended(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        slot: u32,
        pieces: &[PixelRect],
        style: StrokeStyle,
    ) {
        let Some(view) = self.layers.slot_views.get(slot as usize) else {
            return;
        };
        // A zero-sized piece would be an illegal copy extent and draws nothing
        // anyway. `pieces` never holds one today; skipping is cheaper than
        // depending on that.
        let live: Vec<PixelRect> = pieces
            .iter()
            .copied()
            .filter(|p| p.width > 0 && p.height > 0)
            .collect();
        if live.is_empty() {
            return;
        }

        let widest = live.iter().map(|p| p.width).max().unwrap_or(1);
        let tallest = live.iter().map(|p| p.height).max().unwrap_or(1);
        let backdrop = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("umber-commit-backdrop"),
            size: wgpu::Extent3d {
                width: widest,
                height: tallest,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: LAYER_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let backdrop_view = backdrop.create_view(&wgpu::TextureViewDescriptor::default());

        // One uniform block per piece, because the vertex shader spans the
        // piece rather than the whole rectangle: the backdrop copy sits at the
        // texture's origin, so `rect_min` is what maps a fragment into it.
        // Rounded *up to* the alignment rather than `max`ed with it: a dynamic
        // offset must itself be a multiple of the alignment, and a `max` only
        // happens to give one while the block is smaller than 256 bytes. Grow
        // `CommitUniforms` past that and every piece after the first would take
        // an unaligned offset — a validation error on a canvas with two damaged
        // pieces and on no other, which is not the first thing anybody tests.
        let stride = std::mem::size_of::<CommitUniforms>()
            .next_multiple_of(device.limits().min_uniform_buffer_offset_alignment as usize);
        let color = style.color;
        let mut blocks = vec![0u8; stride * live.len()];
        for (i, piece) in live.iter().enumerate() {
            let block = CommitUniforms {
                rect_min: [piece.x as f32, piece.y as f32],
                rect_max: [
                    (piece.x + piece.width) as f32,
                    (piece.y + piece.height) as f32,
                ],
                doc_size: [self.doc_size.x as f32, self.doc_size.y as f32],
                _pad0: [0.0; 2],
                color: [color.r, color.g, color.b, style.opacity.clamp(0.0, 1.0)],
                mode: mode_index(style.mode),
                per_dab_color: u32::from(style.per_dab_color),
                blend: style.blend.index(),
                _pad2: 0,
            };
            let at = i * stride;
            blocks[at..at + std::mem::size_of::<CommitUniforms>()]
                .copy_from_slice(bytemuck::bytes_of(&block));
        }
        let uniforms = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("umber-commit-blend-uniforms"),
            contents: &blocks,
            usage: wgpu::BufferUsages::UNIFORM,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("commit-blend-bg"),
            layout: &self.shared.commit_blend_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &uniforms,
                        offset: 0,
                        // One block, not the whole buffer: with a dynamic
                        // offset the bound range is `offset .. offset + size`,
                        // and binding the lot would run off the end.
                        size: wgpu::BufferSize::new(std::mem::size_of::<CommitUniforms>() as u64),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&self.stroke_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.shared.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&self.stroke_color_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&backdrop_view),
                },
            ],
        });

        for (i, piece) in live.iter().enumerate() {
            // The copy has to precede the pass that reads it, and a copy cannot
            // be recorded inside one. Pieces never overlap, so piece *i* reads
            // pixels no earlier piece wrote.
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.layers.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: piece.x,
                        y: piece.y,
                        z: slot,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &backdrop,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: piece.width,
                    height: piece.height,
                    depth_or_array_layers: 1,
                },
            );

            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("commit-blend-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.shared.commit_blend_pipeline);
            pass.set_bind_group(0, &bind_group, &[(i * stride) as u32]);
            // The quad already covers exactly this piece, so the scissor is
            // belt and braces — and it is what keeps "no pixel outside the
            // pieces the undo patch was captured from is written" a property of
            // the pass rather than of the rasteriser's rounding.
            pass.set_scissor_rect(piece.x, piece.y, piece.width, piece.height);
            pass.draw(0..4, 0..1);
        }
    }

    /// Wipe the scratch surface.
    ///
    /// Both halves of it. Leaving stale colour behind would be the same class
    /// of bug as leaving stale coverage: the next smudging stroke would pick up
    /// the previous one's smear wherever its own dabs had not yet reached.
    pub fn clear_stroke(&self, encoder: &mut wgpu::CommandEncoder) {
        clear_view(encoder, &self.stroke_view, "clear-stroke");
        if self.has_stroke_color {
            clear_view(encoder, &self.stroke_color_view, "clear-stroke-colour");
        }
    }

    /// Wipe one layer.
    pub fn clear_layer(&mut self, encoder: &mut wgpu::CommandEncoder, slot: u32) {
        if let Some(view) = self.layers.slot_views.get(slot as usize) {
            clear_view(encoder, view, "clear-layer");
        }
        self.touch_slot(slot);
    }

    /// Fill one slice with opaque white — what a **new mask** starts as.
    ///
    /// White is "reveal everything", so a layer that has just gained a mask
    /// looks exactly as it did a moment before; that is what makes adding one
    /// something a painter can try rather than commit to.
    ///
    /// A clear rather than a draw. The clear value is linear and the target is
    /// sRGB-typed, but 1.0 encodes to 255 either way, so the slice really does
    /// come back as `0xff` in every channel — which matters, because the
    /// composite reads the red one and a mask that arrived at 0xfe would dim
    /// its layer by a level the painter never asked for.
    pub fn fill_layer_white(&mut self, encoder: &mut wgpu::CommandEncoder, slot: u32) {
        self.touch_slot(slot);
        let Some(view) = self.layers.slot_views.get(slot as usize) else {
            return;
        };
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("fill-mask-white"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }

    /// Wipe every allocated slot. Used at startup.
    pub fn clear_all_layers(&mut self, encoder: &mut wgpu::CommandEncoder) {
        for view in &self.layers.slot_views {
            clear_view(encoder, view, "clear-layer");
        }
        self.touch_all_slots();
    }

    // --- flipping the canvas ------------------------------------------------

    /// Mirror `slots` about the canvas's centre line, in place.
    ///
    /// **Exactly reversible, texel for texel.** That is the requirement, not a
    /// nicety: the history entry a flip records stores no pixels at all and is
    /// undone by flipping again ([`umber_core::EditBody::Flip`]), so anything
    /// lossy here would move the picture a little every time somebody flipped
    /// and undid. `flip.wgsl` has the three things that make it exact — integer
    /// `textureLoad`, non-sRGB views on both sides, and no blending.
    ///
    /// A texture cannot be its own render attachment and
    /// `copy_texture_to_texture` cannot mirror, so each slice is drawn into one
    /// scratch texture and copied straight back. The scratch is canvas-sized
    /// and lives only for the call: a flip is an explicit command, not
    /// something the drawing path does, and holding a spare canvas for the rest
    /// of the session in case somebody presses the key would cost every
    /// document that never does.
    ///
    /// The canvas size does not change, which is the whole reason a flip can
    /// keep the undo history where a resize cannot. Nothing here reallocates.
    ///
    /// The caller owes this **no stroke and no float in flight** — the scratch
    /// surface and the floating copy are not mirrored, so a stroke would commit
    /// unmirrored over the flipped picture and a preview would put its pixels
    /// down in the place they were dragged to before the flip.
    pub fn flip_layers(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        slots: &[u32],
        axis: FlipAxis,
    ) {
        if slots.is_empty() {
            return;
        }
        // A sample recorded against the picture as it was would be read back as
        // though it belonged to the picture as it is.
        self.reset_probes();
        // And a capture part-way through would assemble a file out of layers
        // that were mirrored and layers that were not. The scheduler's half of
        // this is the caller's — see `app.rs`'s `stop_autosave_of`.
        self.cancel_capture();
        for &slot in slots {
            self.touch_slot(slot);
        }

        let scratch = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("umber-flip"),
            size: wgpu::Extent3d {
                width: self.doc_size.x,
                height: self.doc_size.y,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: LAYER_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[LAYER_FORMAT_LINEAR],
        });
        let scratch_view = scratch.create_view(&wgpu::TextureViewDescriptor {
            label: Some("umber-flip-raw"),
            format: Some(LAYER_FORMAT_LINEAR),
            ..Default::default()
        });

        let uniforms = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("flip-uniforms"),
            contents: bytemuck::bytes_of(&FlipUniforms {
                doc_size: [self.doc_size.x, self.doc_size.y],
                axis: match axis {
                    FlipAxis::Horizontal => 0,
                    FlipAxis::Vertical => 1,
                },
                _pad: 0,
            }),
            usage: wgpu::BufferUsages::UNIFORM,
        });

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("flip-canvas"),
        });
        for &slot in slots {
            let Some(source) = self.layers.raw_slot_views.get(slot as usize) else {
                log::error!("flip of slot {slot} beyond capacity");
                continue;
            };
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("flip-bg"),
                layout: &self.shared.flip_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniforms.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(source),
                    },
                ],
            });
            {
                // `Clear` rather than `Load`: every texel of the scratch is
                // written by the pass, so loading whatever the last slice left
                // there would only be a dependency the driver has to honour.
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("flip-slice"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &scratch_view,
                        resolve_target: None,
                        depth_slice: None,
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
                pass.set_pipeline(&self.shared.flip_pipeline);
                pass.set_bind_group(0, &bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
            // A raw byte copy, so the trip back through the array costs the
            // picture nothing. Commands within one encoder run in order, so
            // this reads what the pass above wrote and the next slice's pass
            // may reuse the scratch.
            enc.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &scratch,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &self.layers.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: slot,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: self.doc_size.x,
                    height: self.doc_size.y,
                    depth_or_array_layers: 1,
                },
            );
        }
        queue.submit(Some(enc.finish()));
    }

    // --- floating transforms ------------------------------------------------

    /// The layer whose slice the composite pass must be shown the preview slice
    /// for instead, and that slice — or `None` when nothing is floating.
    ///
    /// This is the whole of how a floating transform reaches the screen. The
    /// caller swaps the slot in its `LayerDraw` for this one and
    /// `composite.wgsl` is untouched: the preview slice holds exactly what the
    /// layer will hold once the float is put down, so it composites at the
    /// right position, under the right blend mode, at the right opacity,
    /// without any of that being restated here. See [`Float`].
    pub fn float_preview(&self) -> Option<(u32, u32)> {
        self.float.as_ref().map(|f| (f.layer_slot, f.preview_slot))
    }

    pub fn float_in_flight(&self) -> bool {
        self.float.is_some()
    }

    /// Pick pixels up off a layer, or put pasted ones down over it, ready to be
    /// dragged about.
    ///
    /// `reserved` is the document's slot high-water mark — everything the layer
    /// stack might use — because the preview needs a slice of the same array
    /// and must not take one a layer could later be given. Returns the preview
    /// slice, or `None` when there is no room for it: a document already using
    /// every slice the shader's array has cannot also hold a preview, and
    /// refusing is better than previewing into a layer.
    ///
    /// Submits twice, deliberately. A paste arrives through
    /// `Queue::write_texture`, whose writes are flushed *before* the command
    /// buffers of the submission they precede — so clearing the floating copy
    /// in the same encoder would wipe the pixels that were just written into
    /// it. The clear therefore goes in its own submission. This runs once per
    /// gesture, where `start_stroke` already submits.
    pub fn begin_float(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        reserved: u32,
        source: &FloatSource<'_>,
    ) -> Option<u32> {
        self.end_float();
        // Against [`MAX_SLOTS`], not [`MAX_LAYERS`]: `reserved` counts *slices*
        // — a layer, a layer's mask — and the array holds twice the stack's
        // entries, plus one, plus the effect-draw headroom. That `+ 1` is this
        // preview's spare. Comparing against 64 refused every document past its
        // 64th slice: 33 masked layers could not be transformed at all, with 63
        // slices free, under a notice that said Umber had run out.
        //
        // **This is reachable**, and used not to be. `reserved` is one past the
        // highest slice *claimed*, and structural undo parks a deleted layer's
        // slice in the entry that could put it back — so a history competes for
        // the range. The caller gives entries up before asking; `App::
        // free_headroom` is that release, and it declines to spend the history
        // where the live stack itself reaches the ceiling, because no eviction
        // can help there. That state needs a live layer holding a slot *number*
        // at the top of the range, which parking puts it there; it is not "64
        // layers each with a mask", which is 128 slices and has never reached
        // any ceiling this constant has had.
        if reserved as usize >= MAX_SLOTS {
            log::error!("no room for a transform preview beside {reserved} layer slices");
            return None;
        }
        let preview_slot = reserved;
        self.ensure_slots(device, queue, preview_slot + 1);
        if source.slot >= self.layers.capacity {
            log::error!("transform of slot {} beyond capacity", source.slot);
            return None;
        }

        let base = self.make_float_texture(device, "umber-float-base");
        let base_view = base.create_view(&wgpu::TextureViewDescriptor::default());
        let floating = self.make_float_texture(device, "umber-float-source");
        let floating_view = floating.create_view(&wgpu::TextureViewDescriptor::default());

        // Snapshotted rather than shared with the dab pass's mask: the two
        // features never run at once, and a binding reached across would tie a
        // live transform to whatever the next stroke set.
        let (mask, mask_min, mask_size, use_mask) = match source.mask {
            Some(sel) => {
                let r = sel.bounds();
                (
                    upload_coverage(
                        device,
                        queue,
                        r.width,
                        r.height,
                        sel.coverage(),
                        "umber-float-mask",
                    ),
                    [r.x as f32, r.y as f32],
                    [r.width as f32, r.height as f32],
                    1,
                )
            }
            // Not zero for the size: the shader divides by it, and a NaN would
            // take the whole quad with it rather than merely being discarded.
            None => (
                make_coverage_texture(device, 1, 1, "umber-float-mask"),
                [0.0, 0.0],
                [1.0, 1.0],
                0,
            ),
        };
        let mask_view = mask.create_view(&wgpu::TextureViewDescriptor::default());

        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("transform-uniforms"),
            size: std::mem::size_of::<TransformUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let make_bind_group = |source: &wgpu::TextureView, label: &str| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &self.shared.transform_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniforms.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.shared.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&mask_view),
                    },
                ],
            })
        };
        let bind_group = make_bind_group(&floating_view, "transform-bg");
        // The mask passes need the layer as it stands, because what they compute
        // is a *share* of what is already there rather than the mask on its own
        // — see `fs_mask`, and the ghost outline that reading the mask alone
        // left behind. It has to be the layer's own slice and not either of
        // their targets: the base and the floating copy are colour attachments
        // here, an exclusive usage, and wgpu refuses a pass that also samples
        // one. The layer is untouched until the commit, so it is the one
        // pristine copy both passes can share.
        let mask_bind_group = make_bind_group(
            &self.layers.slot_views[source.slot as usize],
            "transform-mask-bg",
        );

        // First submission: the floating copy starts empty, whatever the
        // allocation held. See the note on this function.
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("begin-float-clear"),
        });
        clear_view(&mut enc, &floating_view, "clear-float-source");
        queue.submit(Some(enc.finish()));

        let lifting = source.pixels.is_none();
        if let Some(pixels) = source.pixels {
            write_rect(
                queue,
                &floating,
                wgpu::Origin3d {
                    x: source.rect.x,
                    y: source.rect.y,
                    z: 0,
                },
                source.rect,
                pixels,
            );
        }

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("begin-float"),
        });
        // The base is the layer as the float will sit on it. Copied whole
        // because the drag can carry the picture anywhere on the canvas, and
        // every later frame restores out of this.
        enc.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.layers.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: source.slot,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &base,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: self.doc_size.x,
                height: self.doc_size.y,
                depth_or_array_layers: 1,
            },
        );
        if lifting {
            enc.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.layers.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: source.rect.x,
                        y: source.rect.y,
                        z: source.slot,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &floating,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: source.rect.x,
                        y: source.rect.y,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: source.rect.width,
                    height: source.rect.height,
                    depth_or_array_layers: 1,
                },
            );
        }

        // One write, two passes: the mask is applied to the floating copy and
        // its complement to the base, over the same rectangle with the same
        // mask, so both read the same uniforms. Two writes here would be a bug
        // — `write_buffer` is staged, so both passes in one encoder would see
        // whichever was written last.
        queue.write_buffer(
            &uniforms,
            0,
            bytemuck::bytes_of(&TransformUniforms {
                rect_min: [source.rect.x as f32, source.rect.y as f32],
                rect_max: [
                    (source.rect.x + source.rect.width) as f32,
                    (source.rect.y + source.rect.height) as f32,
                ],
                doc_size: [self.doc_size.x as f32, self.doc_size.y as f32],
                inv_x: [1.0, 0.0],
                inv_y: [0.0, 1.0],
                inv_t: [0.0, 0.0],
                mask_min,
                mask_size,
                use_mask,
                _pad0: 0.0,
                _pad1: 0.0,
                _pad2: 0.0,
            }),
        );
        if lifting {
            // A lift outside a selection takes the rectangle whole, and with
            // `use_mask` clear the shader's share is exactly 1.0 — so this pass
            // would be the identity and is skipped rather than run.
            if use_mask != 0 {
                self.mask_pass(
                    &mut enc,
                    &self.shared.transform_keep_pipeline,
                    &mask_bind_group,
                    &floating_view,
                    "float-keep",
                );
            }
            self.mask_pass(
                &mut enc,
                &self.shared.transform_take_pipeline,
                &mask_bind_group,
                &base_view,
                "float-take",
            );
        }
        // The preview starts as the base: the hole is visible the moment the
        // pixels are picked up, before anything has been dragged.
        enc.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &base,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &self.layers.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: preview_slot,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: self.doc_size.x,
                height: self.doc_size.y,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(enc.finish()));

        self.float = Some(Float {
            layer_slot: source.slot,
            preview_slot,
            base,
            source: floating,
            mask,
            uniforms,
            bind_group,
            last_dest: None,
        });
        Some(preview_slot)
    }

    /// Redraw the preview for a transform that has moved.
    ///
    /// Cheap enough for the drawing path: it restores only the rectangle the
    /// previous preview and this one between them cover, and draws only where
    /// the pixels land. Nothing is allocated.
    ///
    /// One uniform write per encoder — see [`Self::begin_float`] — so this and
    /// [`Self::commit_float`] must not share one.
    pub fn draw_float(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        params: &FloatParams,
    ) {
        let Some(float) = self.float.as_ref() else {
            return;
        };
        let (preview_slot, last) = (float.preview_slot, float.last_dest);
        if let Some(restore) = span(last, params.dest) {
            self.render_float(queue, encoder, preview_slot, restore, params);
        }
        if let Some(float) = self.float.as_mut() {
            float.last_dest = params.dest;
        }
    }

    /// Put the floating pixels down into the layer they belong to.
    ///
    /// `damage` must cover the source *and* the destination — see
    /// `Transform::damage` — and the caller must have captured that rectangle
    /// for undo before calling, exactly as `finish_stroke` does.
    ///
    /// This is [`Self::draw_float`]'s own body with the layer's slice as the
    /// target instead of the preview's, which is what makes the committed
    /// result the preview rather than a second rendering of it.
    pub fn commit_float(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        damage: PixelRect,
        params: &FloatParams,
    ) {
        let Some(slot) = self.float.as_ref().map(|f| f.layer_slot) else {
            return;
        };
        self.render_float(queue, encoder, slot, damage, params);
        self.touch_slot(slot);
    }

    /// Give the floating transform's storage back. Nothing is written: the
    /// layer was never touched, so abandoning a transform is exactly this.
    pub fn end_float(&mut self) {
        self.float = None;
    }

    /// Restore `restore` from the base and draw the floating pixels over it,
    /// into the layer-array slice `slot`.
    fn render_float(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        slot: u32,
        restore: PixelRect,
        params: &FloatParams,
    ) {
        let Some(float) = self.float.as_ref() else {
            return;
        };
        let Some(view) = self.layers.slot_views.get(slot as usize) else {
            log::error!("transform into slot {slot} beyond capacity");
            return;
        };
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &float.base,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: restore.x,
                    y: restore.y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &self.layers.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: restore.x,
                    y: restore.y,
                    z: slot,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: restore.width,
                height: restore.height,
                depth_or_array_layers: 1,
            },
        );

        let Some(dest) = params.dest else {
            return;
        };
        let columns = params.inverse.columns();
        queue.write_buffer(
            &float.uniforms,
            0,
            bytemuck::bytes_of(&TransformUniforms {
                rect_min: [dest.x as f32, dest.y as f32],
                rect_max: [(dest.x + dest.width) as f32, (dest.y + dest.height) as f32],
                doc_size: [self.doc_size.x as f32, self.doc_size.y as f32],
                inv_x: columns[0],
                inv_y: columns[1],
                inv_t: columns[2],
                // Unused by `fs_sample`: the mask was applied once, when the
                // pixels were lifted. Applying it again per frame would clip
                // the *moved* picture by where it used to be.
                mask_min: [0.0, 0.0],
                mask_size: [1.0, 1.0],
                use_mask: 0,
                _pad0: 0.0,
                _pad1: 0.0,
                _pad2: 0.0,
            }),
        );

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("float-draw"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.shared.transform_draw_pipeline);
        pass.set_bind_group(0, &float.bind_group, &[]);
        pass.draw(0..4, 0..1);
    }

    /// One of the two mask passes: scale a target by the selection's coverage
    /// or by its complement, over whatever rectangle the uniforms name.
    fn mask_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::RenderPipeline,
        bind_group: &wgpu::BindGroup,
        view: &wgpu::TextureView,
        label: &str,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..4, 0..1);
    }

    /// A canvas-sized texture in layer form, for the two a float holds.
    fn make_float_texture(&self, device: &wgpu::Device, label: &str) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: self.doc_size.x,
                height: self.doc_size.y,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: LAYER_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }

    /// A document-sized offscreen target for the export composite.
    ///
    /// Non-sRGB, matching the real surface: the shader does its own gamma
    /// encode, and an sRGB target would encode twice.
    fn export_target(&self, device: &wgpu::Device) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("umber-export"),
            size: wgpu::Extent3d {
                width: self.doc_size.x,
                height: self.doc_size.y,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OFFSCREEN_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    /// Draw the flattened document 1:1 into `view`.
    ///
    /// Factored out of [`Self::export_rgba`] because the autosave's capture
    /// needs the identical picture and must not block for it. Two spellings of
    /// "the export composite" is exactly how a saved preview starts disagreeing
    /// with an exported PNG.
    fn render_export(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        layers: &[LayerDraw],
    ) {
        // Zoom 1 with the pivot at the document centre makes screen and
        // document coordinates identical, so the render is 1:1.
        let camera = Camera {
            center: Vec2::new(self.doc_size.x as f32 * 0.5, self.doc_size.y as f32 * 0.5),
            zoom: 1.0,
        };
        self.composite(
            queue,
            encoder,
            view,
            &CompositeParams {
                camera: &camera,
                pivot: camera.center,
                layers,
                // No stroke in flight: exporting mid-stroke should write what
                // is committed, not a half-finished dab.
                active_index: 0,
                stroke: StrokeStyle {
                    opacity: 0.0,
                    ..Default::default()
                },
                backdrop: [0.0, 0.0, 0.0],
                export: true,
            },
        );
    }

    /// Flatten the visible stack to straight-alpha sRGB bytes, document-sized.
    ///
    /// Runs the same composite pass the screen uses, with its export flag set,
    /// so what lands in the file is what the canvas showed. A separate export
    /// path would be a second copy of the blend maths to keep in step.
    ///
    /// The document background is part of that, so a white-backed document
    /// exports opaque and a transparent one keeps its alpha, without this
    /// function knowing which it is.
    pub fn export_rgba(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layers: &[LayerDraw],
    ) -> Vec<u8> {
        let (w, h) = (self.doc_size.x, self.doc_size.y);

        let target = self.export_target(device);
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("export"),
        });
        self.render_export(queue, &mut encoder, &view, layers);
        // Submitted before the readback rather than sharing its encoder: the
        // readback may take several submits, and the flatten has to happen once
        // and before all of them.
        queue.submit(Some(encoder.finish()));

        self.read_texture_rows(
            device,
            queue,
            "export",
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            (w, h),
        )
    }

    /// Sample the flattened stack at one document pixel.
    ///
    /// Renders a 1×1 target rather than the whole document: an eyedropper only
    /// needs one pixel, and flattening 2048² to read four bytes would stall for
    /// milliseconds on every click. Uses the same composite pass as the screen,
    /// so the sampled colour is the one the user is looking at rather than the
    /// contents of whichever layer happens to be selected.
    ///
    /// Returns straight-alpha sRGB. A fully transparent pixel yields alpha 0,
    /// which the caller should treat as "nothing there" rather than as black.
    pub fn pick_colour(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layers: &[LayerDraw],
        doc_point: Vec2,
    ) -> [u8; 4] {
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("umber-pick"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OFFSCREEN_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());

        // The lone fragment sits at screen (0.5, 0.5); with zoom 1 and the
        // pivot there, that maps exactly to `doc_point`.
        let camera = Camera {
            center: doc_point,
            zoom: 1.0,
        };

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("pick"),
        });
        self.composite(
            queue,
            &mut encoder,
            &view,
            &CompositeParams {
                camera: &camera,
                pivot: Vec2::splat(0.5),
                layers,
                active_index: 0,
                stroke: StrokeStyle {
                    opacity: 0.0,
                    ..Default::default()
                },
                backdrop: [0.0, 0.0, 0.0],
                export: true,
            },
        );

        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pick-readback"),
            size: wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT),
                    rows_per_image: Some(1),
                },
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));

        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        let mapped = slice.get_mapped_range();
        let px = [mapped[0], mapped[1], mapped[2], mapped[3]];
        drop(mapped);
        staging.unmap();
        px
    }

    /// Ask what the canvas looks like under the brush, without waiting for it.
    ///
    /// This is [`Self::pick_colour`] with the blocking removed, and it exists
    /// for one caller: a smudging brush, which needs the canvas colour on every
    /// frame of a stroke. `pick_colour` blocks on the GPU, and a blocking read
    /// per frame during a stroke is exactly the thing this project is built to
    /// avoid — `read_layer_rect` carries the same warning for the same reason.
    ///
    /// So the answer arrives a frame or two later, through [`Self::take_probe`].
    /// The stroke it feeds is a trailing average by definition, and MyPaint's
    /// own smudge lags far more than the readback does, so the delay costs
    /// nothing visible.
    ///
    /// The composite pass is reused with the stroke *included*, so a blender
    /// scrubbed back and forth picks up its own wet paint rather than only what
    /// was on the layer when the stroke started.
    pub fn probe_canvas(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        params: &ProbeParams<'_>,
    ) {
        let ProbeParams {
            layers,
            active_index,
            stroke,
            doc_point,
            radius,
        } = *params;
        // A slot disowned by the previous stroke can come back into service as
        // soon as its map has settled, which is usually by now.
        self.reclaim_stale();
        // By index rather than by reference: `composite` below needs `&self`,
        // and a live `&mut` into `self.probes` would still be outstanding.
        let Some(index) = self.probes.iter().position(|p| p.state == ProbeState::Idle) else {
            // Every slot is still in flight. Dropping this sample is right: the
            // ones outstanding are more recent than anything a queue would hold,
            // and a smudge that lags further is worse than one that samples less
            // often.
            return;
        };
        self.probes[index].state = ProbeState::Rendering;

        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("umber-probe"),
            size: wgpu::Extent3d {
                width: PROBE_SIZE,
                height: PROBE_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: PROBE_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());

        // Zoom so the brush's footprint fills the little target exactly, which
        // makes the readback an area average over what the dab covers rather
        // than a point sample that a single stray pixel could dominate.
        let camera = Camera {
            center: doc_point,
            zoom: PROBE_SIZE as f32 / (radius * 2.0).max(0.5),
        };
        self.composite(
            queue,
            encoder,
            &view,
            &CompositeParams {
                camera: &camera,
                pivot: Vec2::splat(PROBE_SIZE as f32 * 0.5),
                layers,
                active_index,
                stroke,
                backdrop: [0.0, 0.0, 0.0],
                // The export path returns straight alpha and skips the sRGB
                // encode, which is what makes the result usable as linear
                // colour with a meaningful alpha. Exactly what `pick_colour`
                // relies on, for the same reason.
                export: true,
            },
        );

        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.probes[index].buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(PROBE_ROW_BYTES),
                    rows_per_image: Some(PROBE_SIZE),
                },
            },
            wgpu::Extent3d {
                width: PROBE_SIZE,
                height: PROBE_SIZE,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Start the map for any probe whose copy has been submitted.
    ///
    /// Split from [`Self::probe_canvas`] because `map_async` may only be called
    /// on a buffer whose writes are already submitted, and the encoder holding
    /// that copy is still open when the probe is recorded.
    pub fn submit_probes(&mut self) {
        for slot in &mut self.probes {
            if slot.state != ProbeState::Rendering {
                continue;
            }
            slot.state = ProbeState::Mapping;
            slot.outcome.store(PROBE_PENDING, Ordering::Release);
            let outcome = slot.outcome.clone();
            slot.buffer
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    // A failed map is recorded rather than ignored: the slot
                    // still has to go back into service, but unmapping a buffer
                    // that was never mapped is itself an error. A smudge that
                    // misses a sample is a cosmetic loss, not a reason to take
                    // the app down.
                    let code = if result.is_ok() {
                        PROBE_MAPPED
                    } else {
                        PROBE_FAILED
                    };
                    outcome.store(code, Ordering::Release);
                });
        }
    }

    /// Collect whichever probe has come home, averaged to one linear RGBA.
    ///
    /// Polls without blocking — `PollType::Poll` returns immediately whether or
    /// not the GPU has caught up, which is the entire point.
    pub fn take_probe(&mut self, device: &wgpu::Device) -> Option<[f32; 4]> {
        let _ = device.poll(wgpu::PollType::Poll);

        let mut out = None;
        for slot in &mut self.probes {
            if slot.state != ProbeState::Mapping {
                continue;
            }
            match slot.outcome.load(Ordering::Acquire) {
                PROBE_MAPPED => {
                    // A sample belonging to a stroke that has already ended is
                    // read back and thrown away: the buffer still has to be
                    // unmapped before the slot can be used again.
                    if !slot.stale {
                        let mapped = slot.buffer.slice(..).get_mapped_range();
                        out = Some(average_probe(&mapped));
                    }
                    slot.buffer.unmap();
                }
                PROBE_FAILED => {}
                // Still in flight. Leaving it alone is the whole point — see
                // `reset_probes`.
                _ => continue,
            }
            slot.outcome.store(PROBE_PENDING, Ordering::Release);
            slot.stale = false;
            slot.state = ProbeState::Idle;
        }
        out
    }

    /// Disown every probe in flight, so no sample of the stroke that is ending
    /// can reach the next one.
    ///
    /// Note what this does *not* do: return a slot whose `map_async` is still
    /// outstanding to service. Doing that was a real crash. `probe_canvas`
    /// would hand the next stroke that slot, record a copy into it, and
    /// `queue.submit` refuses any submission touching a buffer that is mapped
    /// or awaiting a map — which is a validation error, and a validation error
    /// aborts the process. It is also the ordinary case rather than a rare one:
    /// a map only completes on a poll, so a stroke that ends between frames
    /// almost always leaves one behind.
    ///
    /// So the slot stays where it is and is merely marked stale;
    /// [`Self::take_probe`] unmaps it and returns it to service once the GPU is
    /// done with it.
    pub fn reset_probes(&mut self) {
        for slot in &mut self.probes {
            // `Rendering` means a copy is recorded but `map_async` has not been
            // called yet; the next `submit_probes` maps it and `take_probe`
            // then discards it, for the same reason.
            if slot.state != ProbeState::Idle {
                slot.stale = true;
            }
        }
        self.reclaim_stale();
    }

    /// Free any disowned slot whose map has already settled.
    ///
    /// Without this a stale slot would wait on [`Self::take_probe`], which
    /// `app.rs` only calls while a *smudging* stroke is live. Ending a smudge
    /// and then picking an ordinary brush would leave both slots parked for the
    /// rest of the session, and the next blender would never sample anything.
    fn reclaim_stale(&mut self) {
        for slot in &mut self.probes {
            if slot.state != ProbeState::Mapping || !slot.stale {
                continue;
            }
            match slot.outcome.load(Ordering::Acquire) {
                PROBE_MAPPED => slot.buffer.unmap(),
                PROBE_FAILED => {}
                _ => continue,
            }
            slot.outcome.store(PROBE_PENDING, Ordering::Release);
            slot.stale = false;
            slot.state = ProbeState::Idle;
        }
    }

    // --- layer thumbnails ---------------------------------------------------

    /// How many times slot `slot` has been written to.
    ///
    /// The layer list's whole invalidation rule: a thumbnail is stale exactly
    /// when this has moved since it was taken. See
    /// [`CanvasRenderer::slot_revisions`] for why the counter lives here.
    pub fn slot_revision(&self, slot: u32) -> u64 {
        self.slot_revisions.get(slot as usize).copied().unwrap_or(0)
    }

    /// Note that a slice's pixels have changed.
    ///
    /// Called by every method here that writes one, and by nothing outside this
    /// type. A thumbnail of that slice in flight is disowned in the same
    /// breath: it is a picture of the layer as it was a moment ago, and drawing
    /// it would show the stroke that has just landed as missing.
    fn touch_slot(&mut self, slot: u32) {
        if let Some(rev) = self.slot_revisions.get_mut(slot as usize) {
            *rev += 1;
        }
        if let Some(job) = self.thumb.as_mut()
            && job.slot == slot
        {
            job.abandoned = true;
        }
    }

    /// Note that every slice has changed — a flip, a resize, a fresh document.
    fn touch_all_slots(&mut self) {
        for rev in &mut self.slot_revisions {
            *rev += 1;
        }
        self.cancel_thumb();
    }

    /// True while a thumbnail is in flight, abandoned or otherwise.
    ///
    /// Abandoned counts, for the reason [`Self::capture_in_flight`] says: the
    /// staging buffer is the GPU's until its map settles.
    pub fn thumb_in_flight(&self) -> bool {
        self.thumb.is_some()
    }

    /// True when the thumbnail in flight has finished its bounds pass and is
    /// waiting to draw its picture.
    ///
    /// Exists for one test — the one that pins what happens when a layer is
    /// written *between* the two passes, which is the gap a stroke's commit
    /// lands in every time. Nothing in the application asks.
    #[doc(hidden)]
    pub fn thumb_phase_is_picture(&self) -> bool {
        self.thumb
            .as_ref()
            .is_some_and(|job| job.phase == ThumbPhase::Picture)
    }

    /// Start reading a thumbnail of `slot` back, without blocking.
    ///
    /// Returns false when one is already in flight — the caller's cue to ask
    /// again next frame rather than to queue a second. Nothing is recorded
    /// here: [`Self::drive_thumb`] records a pass, [`Self::submit_thumb`] maps
    /// it, and [`Self::take_thumb`] collects it and lets the next pass go.
    pub fn begin_thumb(&mut self, slot: u32) -> bool {
        if self.thumb.is_some() || slot >= self.layers.capacity {
            return false;
        }
        self.thumb = Some(ThumbJob {
            slot,
            revision: self.slot_revision(slot),
            phase: ThumbPhase::Bounds,
            region: None,
            state: StepState::Waiting,
            outcome: Arc::new(AtomicU8::new(PROBE_PENDING)),
            abandoned: false,
        });
        true
    }

    /// Record the pass this thumbnail is waiting on, into the frame's encoder.
    ///
    /// Costs one draw over 64² fragments and one 16 KB copy. The draw reads
    /// every texel of the region exactly once between them, which is the same
    /// bandwidth the composite pass spends on that layer every frame anyway.
    pub fn drive_thumb(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder) {
        let Some(job) = self.thumb.as_ref() else {
            return;
        };
        if job.state != StepState::Waiting || job.abandoned {
            return;
        }
        let (slot, phase, region) = (job.slot, job.phase, job.region);
        // The whole slice for the bounds pass; what that found for the picture.
        let region = match (phase, region) {
            (ThumbPhase::Bounds, _) => umber_core::Rect::new(
                Vec2::ZERO,
                Vec2::new(self.doc_size.x as f32, self.doc_size.y as f32),
            ),
            (ThumbPhase::Picture, Some(region)) => region,
            // A picture phase with no region cannot arise — `take_thumb` sets
            // one or finishes the job — but a silent wrong picture is worse
            // than a dropped one.
            (ThumbPhase::Picture, None) => return,
        };

        let target = self.thumb_target.get_or_insert_with(|| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("umber-thumbnail"),
                size: wgpu::Extent3d {
                    width: THUMB_SIZE,
                    height: THUMB_SIZE,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: OFFSCREEN_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let buffer = self.thumb_buffer.get_or_insert_with(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("umber-thumbnail-readback"),
                size: (THUMB_ROW_BYTES * THUMB_SIZE) as u64,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });

        let uniforms = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("thumb-uniforms"),
            contents: bytemuck::bytes_of(&ThumbUniforms {
                src_min: [region.min.x, region.min.y],
                src_size: [region.max.x - region.min.x, region.max.y - region.min.y],
                dest: [THUMB_SIZE, THUMB_SIZE],
                layer_size: [self.doc_size.x, self.doc_size.y],
                slot,
                reduce: u32::from(phase == ThumbPhase::Bounds),
                _pad0: 0,
                _pad1: 0,
            }),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("thumb-bg"),
            layout: &self.shared.thumb_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&self.layers.array_view),
                },
            ],
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("thumbnail-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        // Every texel is written by the draw, so loading the
                        // last job's picture would only be a dependency the
                        // driver has to honour.
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.shared.thumb_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(THUMB_ROW_BYTES),
                    rows_per_image: Some(THUMB_SIZE),
                },
            },
            wgpu::Extent3d {
                width: THUMB_SIZE,
                height: THUMB_SIZE,
                depth_or_array_layers: 1,
            },
        );

        if let Some(job) = self.thumb.as_mut() {
            job.state = StepState::Rendering;
        }
    }

    /// Start the map for a thumbnail pass whose copy has been submitted.
    ///
    /// Split from [`Self::drive_thumb`] for the reason [`Self::submit_probes`]
    /// is split from `probe_canvas`: `map_async` may only be called on a buffer
    /// whose writes are already submitted, and the encoder holding that copy is
    /// still open when the pass is recorded.
    pub fn submit_thumb(&mut self) {
        let Some(job) = self.thumb.as_mut() else {
            return;
        };
        if job.state != StepState::Rendering {
            return;
        }
        let Some(buffer) = self.thumb_buffer.as_ref() else {
            return;
        };
        job.state = StepState::Mapping;
        job.outcome.store(PROBE_PENDING, Ordering::Release);
        let outcome = job.outcome.clone();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let code = if result.is_ok() {
                    PROBE_MAPPED
                } else {
                    PROBE_FAILED
                };
                outcome.store(code, Ordering::Release);
            });
    }

    /// Collect a thumbnail that has come home, if one has.
    ///
    /// Polls without blocking. A `Some` is the picture; the bounds pass in
    /// between produces nothing and merely arms the second pass, so a caller
    /// asks every frame and gets an answer every few.
    pub fn take_thumb(&mut self, device: &wgpu::Device) -> Option<Thumbnail> {
        let _ = device.poll(wgpu::PollType::Poll);

        // A disowned job is dropped as soon as nothing is outstanding on the
        // GPU, **whatever state it is in** — which is the whole of why this is
        // here rather than folded into the `Mapping` arm below.
        //
        // The bounds pass leaves the job `Waiting` at the end of one frame and
        // the picture pass is not recorded until the next, so every route that
        // writes a layer — a stroke committing, an undo, a clear, a flip —
        // lands in that gap routinely and marks it. Left in `Waiting` it would
        // be refused by `drive_thumb` (abandoned), by `submit_thumb` (not
        // `Rendering`) and by the test below (not `Mapping`), so `self.thumb`
        // would stay `Some` for the life of the renderer: no thumbnail would
        // ever update again, and `thumb_in_flight` would request a redraw every
        // frame for ever — the exact "the app never gets to wait" regression
        // `render`'s `repaint_at` exists to prevent. `take_capture` has always
        // checked its own flag at the top for the same reason.
        let job = self.thumb.as_mut()?;
        if job.abandoned && job.state != StepState::Mapping {
            // `Rendering` means a copy is recorded but no map is outstanding,
            // so the buffer is free the moment nothing intends to map it —
            // and dropping the job is what stops `submit_thumb` doing so.
            self.thumb = None;
            return None;
        }
        if job.state != StepState::Mapping {
            return None;
        }
        let buffer = self.thumb_buffer.as_ref()?;
        let mut bytes = None;
        match job.outcome.load(Ordering::Acquire) {
            PROBE_MAPPED => {
                // Read even when abandoned: the buffer still has to be unmapped
                // before the next job can be given it, and reading is what
                // makes the unmap legal to reason about.
                if !job.abandoned {
                    bytes = Some(buffer.slice(..).get_mapped_range().to_vec());
                }
                buffer.unmap();
            }
            PROBE_FAILED => {}
            // Still in flight. Leaving it alone is the whole point.
            _ => return None,
        }
        job.outcome.store(PROBE_PENDING, Ordering::Release);
        job.state = StepState::Waiting;

        let (slot, revision, phase, abandoned) = (job.slot, job.revision, job.phase, job.abandoned);
        let Some(bytes) = bytes else {
            // Abandoned or failed: the buffer is back, so the job goes.
            self.thumb = None;
            return None;
        };
        if abandoned {
            self.thumb = None;
            return None;
        }

        match phase {
            ThumbPhase::Bounds => {
                let content = umber_core::thumbnail::content_rect(
                    &bytes,
                    THUMB_ROW_BYTES as usize,
                    UVec2::splat(THUMB_SIZE),
                    self.doc_size,
                );
                match content {
                    Some(content) => {
                        let job = self.thumb.as_mut()?;
                        job.region = Some(umber_core::thumbnail::framed(
                            content,
                            UVec2::splat(THUMB_SIZE),
                        ));
                        job.phase = ThumbPhase::Picture;
                        None
                    }
                    // Nothing on the layer. Answered rather than left to time
                    // out, so the list can draw its "empty" state and stop
                    // asking: a job that produced no answer would be requested
                    // again on the very next frame, for ever.
                    None => {
                        self.thumb = None;
                        Some(Thumbnail {
                            slot,
                            revision,
                            rgba: Vec::new(),
                        })
                    }
                }
            }
            ThumbPhase::Picture => {
                self.thumb = None;
                Some(Thumbnail {
                    slot,
                    revision,
                    rgba: bytes,
                })
            }
        }
    }

    /// Disown the thumbnail in flight, if there is one.
    ///
    /// Marked rather than dropped, for the reason [`Self::reset_probes`] gives
    /// at length: a buffer awaiting a map is still the GPU's, and recording a
    /// copy into one is a validation error and therefore an abort.
    pub fn cancel_thumb(&mut self) {
        if let Some(job) = self.thumb.as_mut() {
            job.abandoned = true;
        }
    }

    // --- the whole-document capture ----------------------------------------

    /// True while a capture is in flight, abandoned or otherwise.
    ///
    /// Abandoned counts: the staging buffer is still the GPU's until its map
    /// settles, so a second capture would allocate beside it rather than
    /// instead of it.
    pub fn capture_in_flight(&self) -> bool {
        self.capture.is_some()
    }

    /// Start reading the whole document back, without blocking.
    ///
    /// `slots` are the texture-array slices to read, in stack order; `draws` is
    /// the same stack the composite pass takes, for the flattened preview.
    /// Returns false when one is already in flight — the caller's cue to try
    /// again later rather than to queue a second.
    ///
    /// Nothing is copied here. [`Self::drive_capture`] records one step,
    /// [`Self::submit_capture`] maps it, and [`Self::take_capture`] collects it
    /// and lets the next step go. See [`Capture`] for why it is spread out.
    pub fn begin_capture(&mut self, slots: &[u32], draws: &[LayerDraw]) -> bool {
        if self.capture.is_some() || slots.is_empty() {
            return false;
        }
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = (self.doc_size.x * 4).div_ceil(align) * align;
        self.capture = Some(Capture {
            size: self.doc_size,
            padded,
            slots: slots.to_vec(),
            draws: draws.to_vec(),
            buffer: None,
            band: 0,
            row: 0,
            outcome: Arc::new(AtomicU8::new(PROBE_PENDING)),
            state: StepState::Waiting,
            step: 0,
            // One per layer, and one for the flattened preview the format
            // requires.
            results: (0..slots.len() + 1).map(|_| None).collect(),
            partial: None,
            merged_target: None,
            abandoned: false,
            failed: false,
        });
        true
    }

    /// Record the next step's copy into this frame's encoder, if the staging
    /// buffer is free.
    ///
    /// Costs one `copy_texture_to_buffer` — or, for the last step, one
    /// composite into an offscreen target and then the copy. Both are
    /// *recorded*, not executed: nothing on this path waits for the GPU.
    pub fn drive_capture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        // Taken out because the preview's composite borrows `self` — putting it
        // back is unconditional, so the job cannot be lost down any path.
        let Some(mut job) = self.capture.take() else {
            return;
        };
        if job.abandoned || job.failed || job.state != StepState::Waiting || job.complete() {
            self.capture = Some(job);
            return;
        }

        let index = job.step;
        job.band = band_rows(self.readback_limit, job.padded, job.size.y);
        let (band_first, band_last) = job.band_span();
        let height = band_last as u32 - band_first as u32;
        // Allocated once and reused for every band of every step. A buffer per
        // layer would be the document's own size in staging memory on top of the
        // copy of it being assembled.
        let buffer = job.buffer.take().unwrap_or_else(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("umber-capture"),
                size: (job.padded as u64) * (job.band as u64),
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });

        let source = if index < job.slots.len() {
            let slot = job.slots[index];
            if slot >= self.layers.capacity {
                // Cannot happen — `ensure_slots` runs before a layer is ever
                // painted — but a copy naming a slice the array does not have
                // is a validation error, and a validation error aborts the
                // process. An autosave is not worth taking the app down for.
                log::error!("capture named slot {slot} beyond capacity");
                job.failed = true;
                job.buffer = Some(buffer);
                self.capture = Some(job);
                return;
            }
            wgpu::TexelCopyTextureInfo {
                texture: &self.layers.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: band_first as u32,
                    z: slot,
                },
                aspect: wgpu::TextureAspect::All,
            }
        } else {
            // The flattened preview, from the *same* composite pass the screen
            // uses — the reason `export_rgba` and `pick_colour` reuse it too.
            // A second copy of the blend maths here would be a second thing to
            // keep in step, and a preview that disagreed with the screen is the
            // bug that arrangement produces.
            //
            // Drawn once per *step*, not once per band: the later bands of a
            // banded capture read further down the same flattened image, so
            // re-compositing would be the whole document's blend maths run again
            // to fetch rows that are already sitting there.
            if job.merged_target.is_none() {
                let target = self.export_target(device);
                let view = target.create_view(&wgpu::TextureViewDescriptor::default());
                self.render_export(queue, encoder, &view, &job.draws);
                job.merged_target = Some(target);
            }
            wgpu::TexelCopyTextureInfo {
                // Held in `job.merged_target` for the rest of this function.
                texture: job.merged_target.as_ref().expect("just set"),
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: band_first as u32,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            }
        };

        encoder.copy_texture_to_buffer(
            source,
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(job.padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width: job.size.x,
                height,
                depth_or_array_layers: 1,
            },
        );

        job.buffer = Some(buffer);
        job.state = StepState::Rendering;
        self.capture = Some(job);
    }

    /// Map whatever [`Self::drive_capture`] recorded, once the frame holding it
    /// has been submitted.
    ///
    /// Separate from recording for the same reason [`Self::submit_probes`] is:
    /// `map_async` on a buffer whose copy has not been submitted would map it
    /// before the GPU has written to it.
    pub fn submit_capture(&mut self) {
        let Some(job) = self.capture.as_mut() else {
            return;
        };
        if job.state != StepState::Rendering {
            return;
        }
        let Some(buffer) = job.buffer.as_ref() else {
            return;
        };
        job.state = StepState::Mapping;
        job.outcome.store(PROBE_PENDING, Ordering::Release);
        let outcome = job.outcome.clone();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                // Recorded rather than ignored, for the reason the probe's
                // callback records it: a failed map leaves the buffer unmapped,
                // and unmapping one that was never mapped is itself an error.
                let code = if result.is_ok() {
                    PROBE_MAPPED
                } else {
                    PROBE_FAILED
                };
                outcome.store(code, Ordering::Release);
            });
        // The preview's offscreen target is dropped by `take_capture` once the
        // last band has been copied out of it; holding it any longer would keep
        // a canvas-sized texture alive for the rest of the readback. It cannot
        // go here any more, because a banded capture comes back for the rows
        // below this one.
    }

    /// Collect the step that has come home, and hand back the document once the
    /// last of them has.
    ///
    /// Polls without blocking, like [`Self::take_probe`]. At most one layer's
    /// worth of bytes is copied out per call, which is what keeps the cost of
    /// an autosave to one memcpy in the frames that have one at all.
    pub fn take_capture(&mut self, device: &wgpu::Device) -> Option<DocumentCapture> {
        self.capture.as_ref()?;
        let _ = device.poll(wgpu::PollType::Poll);
        let job = self.capture.as_mut()?;

        if job.state == StepState::Mapping {
            match job.outcome.load(Ordering::Acquire) {
                PROBE_MAPPED => {
                    // A capture that has been abandoned still has to unmap, but
                    // there is no point reading sixteen megabytes out of it.
                    let dropped = job.abandoned || job.failed;
                    let band_done = if dropped {
                        job.partial = None;
                        true
                    } else {
                        job.copy_chunk()
                    };
                    if band_done {
                        job.buffer
                            .as_ref()
                            .expect("a mapped step has its buffer")
                            .unmap();
                        // A step is one band on any ordinary document and
                        // several on one too large for the device's staging
                        // limit. Only when the last of them is out does the
                        // layer count as read.
                        if !dropped {
                            job.row += job.band;
                            if job.step_done() {
                                job.row = 0;
                                if let Some(bytes) = job.partial.take() {
                                    job.results[job.step] = Some(bytes);
                                    job.step += 1;
                                }
                                // The flattened preview has been read out of;
                                // give the canvas-sized texture back.
                                job.merged_target = None;
                            }
                        }
                        job.state = StepState::Waiting;
                    }
                }
                // Nothing to read and nothing to unmap. The whole capture goes:
                // a document missing one layer is not a shorter document, it is
                // a wrong one.
                PROBE_FAILED => {
                    job.failed = true;
                    job.state = StepState::Waiting;
                }
                // Still in flight. Leaving it alone is the whole point.
                _ => {}
            }
        }

        if job.abandoned || job.failed {
            if job.settled() {
                if job.failed {
                    log::warn!("a document capture could not be read back; nothing was written");
                }
                self.capture = None;
            }
            return None;
        }
        if !job.complete() {
            return None;
        }

        let job = self.capture.take().expect("checked above");
        let size = job.size;
        let mut buffers: Vec<Vec<u8>> = job
            .results
            .into_iter()
            .map(|r| r.expect("a complete capture has every buffer"))
            .collect();
        let merged = buffers.pop().expect("the preview is the last step");
        Some(DocumentCapture {
            size,
            layers: buffers,
            merged,
        })
    }

    /// Disown a capture in flight, because what it is reading is about to stop
    /// being true — a resize, or the document being closed.
    ///
    /// Note what this does *not* do: free the buffer. A `map_async` that is
    /// still outstanding makes its buffer untouchable, and dropping the job
    /// here is the same class of mistake [`Self::reset_probes`] documents. The
    /// job stays until [`Self::take_capture`] finds it settled.
    pub fn cancel_capture(&mut self) {
        if let Some(job) = self.capture.as_mut() {
            job.abandoned = true;
        }
    }

    /// Pretend this device will not allocate a staging buffer larger than
    /// `bytes`, so the banded readback path can be driven on a document small
    /// enough to check by hand.
    ///
    /// Exists for the tests. Reaching the real limit takes a canvas of about
    /// 8192², which is more memory than a test should ask a CI runner for — and
    /// an untested path that only the largest documents take is the one that
    /// silently returns a sheared picture.
    pub fn set_readback_limit(&mut self, bytes: u64) {
        self.readback_limit = bytes;
    }

    /// Read a rectangle of one layer back to the CPU, for the undo stack.
    ///
    /// This blocks until the GPU catches up. That is acceptable because it runs
    /// once per stroke at pointer-up, never inside the drawing loop. An
    /// autosave must not use it — see [`Capture`].
    pub fn read_layer_rect(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        slot: u32,
        rect: PixelRect,
    ) -> Vec<u8> {
        // A copy naming a slice the array does not have is a validation error,
        // and a validation error aborts the process. It should not happen —
        // `ensure_slots` runs before a layer is ever painted — but "should not"
        // is not a reason to make it fatal, and the resume path rebuilds
        // storage from scratch with the stack already deep. An all-zero patch
        // is the same thing an untouched layer would have read back.
        if slot >= self.layers.capacity {
            log::error!(
                "read from slot {slot} beyond capacity {}",
                self.layers.capacity
            );
            return vec![0; (rect.width as u64 * 4 * rect.height as u64) as usize];
        }
        self.read_texture_rows(
            device,
            queue,
            "undo",
            wgpu::TexelCopyTextureInfo {
                texture: &self.layers.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: rect.x,
                    y: rect.y,
                    z: slot,
                },
                aspect: wgpu::TextureAspect::All,
            },
            (rect.width, rect.height),
        )
    }

    /// Read several rectangles of one layer back to the CPU, for the undo
    /// stack, in **one** submission and one wait.
    ///
    /// This is what a stroke's patch is captured with. The pieces are the cells
    /// of the canvas the stroke actually reached
    /// ([`umber_core::damage::TileMask`]), which for a diagonal across a large
    /// document is a hundred and fifty separate rectangles — and a hundred and
    /// fifty calls to [`Self::read_layer_rect`] would be a hundred and fifty
    /// submissions each blocking on its own fence, at pointer-up, in front of
    /// the artist. Recorded together they cost one.
    ///
    /// Blocking, like [`Self::read_layer_rect`] and for the same reason: once
    /// per stroke is acceptable, the drawing loop is not.
    pub fn read_layer_pieces(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        slot: u32,
        pieces: &[PixelRect],
    ) -> Vec<Vec<u8>> {
        // As in `read_layer_rect`: refuse rather than abort. See there.
        if slot >= self.layers.capacity {
            log::error!(
                "read from slot {slot} beyond capacity {}",
                self.layers.capacity
            );
            return pieces
                .iter()
                .map(|r| vec![0; (r.area() * 4) as usize])
                .collect();
        }

        let align = u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        // Every block is a whole number of padded rows, so laying them end to
        // end keeps each one's offset aligned without any arithmetic of its
        // own.
        let block =
            |r: &PixelRect| (u64::from(r.width) * 4).div_ceil(align) * align * u64::from(r.height);

        let mut out: Vec<Vec<u8>> = Vec::with_capacity(pieces.len());
        let mut batch: Vec<&PixelRect> = Vec::new();
        let mut used = 0u64;
        let flush = |batch: &mut Vec<&PixelRect>, out: &mut Vec<Vec<u8>>| {
            if !batch.is_empty() {
                self.read_batch(device, queue, slot, batch, out);
                batch.clear();
            }
        };

        for piece in pieces {
            let size = block(piece);
            // One piece larger than the whole limit cannot be batched at all.
            // Cell runs are at most one cell tall, so this needs a patch that
            // did not come from a damage mask — but the banded path exists and
            // costs nothing to fall back to.
            if size > self.readback_limit {
                flush(&mut batch, &mut out);
                out.push(self.read_layer_rect(device, queue, slot, *piece));
                continue;
            }
            if used + size > self.readback_limit {
                flush(&mut batch, &mut out);
                used = 0;
            }
            used += size;
            batch.push(piece);
        }
        flush(&mut batch, &mut out);
        out
    }

    /// One submission's worth of [`Self::read_layer_pieces`]: every piece
    /// copied into one staging buffer, then mapped once.
    fn read_batch(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        slot: u32,
        pieces: &[&PixelRect],
        out: &mut Vec<Vec<u8>>,
    ) {
        let align = u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let padded = |r: &PixelRect| (u64::from(r.width) * 4).div_ceil(align) * align;

        let mut offsets = Vec::with_capacity(pieces.len());
        let mut size = 0u64;
        for piece in pieces {
            offsets.push(size);
            size += padded(piece) * u64::from(piece.height);
        }

        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("undo-pieces-readback"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("undo-pieces"),
        });
        for (piece, offset) in pieces.iter().zip(&offsets) {
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.layers.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: piece.x,
                        y: piece.y,
                        z: slot,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &staging,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: *offset,
                        bytes_per_row: Some(padded(piece) as u32),
                        rows_per_image: Some(piece.height),
                    },
                },
                wgpu::Extent3d {
                    width: piece.width,
                    height: piece.height,
                    depth_or_array_layers: 1,
                },
            );
        }
        queue.submit(Some(encoder.finish()));

        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        let mapped = slice.get_mapped_range();
        for (piece, offset) in pieces.iter().zip(&offsets) {
            let unpadded = (piece.width * 4) as usize;
            let padded = padded(piece) as usize;
            let mut bytes = Vec::with_capacity(unpadded * piece.height as usize);
            for row in 0..piece.height as usize {
                let start = *offset as usize + row * padded;
                bytes.extend_from_slice(&mapped[start..start + unpadded]);
            }
            out.push(bytes);
        }
        drop(mapped);
        staging.unmap();
    }

    /// Copy a rectangle of a texture back to the CPU, blocking, and return it
    /// tightly packed — the 256-byte row padding the copy requires stripped.
    ///
    /// Goes a band of rows at a time, because a document can be larger than the
    /// largest buffer the device will allocate; see [`band_rows`]. One buffer is
    /// made and reused for every band, so the banded path costs extra submits
    /// and nothing else. Both blocking readbacks share this: a second copy of
    /// the padding arithmetic is a second place for a document to come back
    /// sheared.
    fn read_texture_rows(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &str,
        source: wgpu::TexelCopyTextureInfo<'_>,
        size: (u32, u32),
    ) -> Vec<u8> {
        let (width, height) = size;
        let unpadded = width * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;
        let band = band_rows(self.readback_limit, padded, height);

        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("{label}-readback")),
            size: (padded as u64) * (band as u64),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut out = Vec::with_capacity((unpadded as usize) * (height as usize));
        let mut first = 0;
        while first < height {
            let rows = band.min(height - first);
            let mut encoder = device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    origin: wgpu::Origin3d {
                        y: source.origin.y + first,
                        ..source.origin
                    },
                    ..source
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &staging,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(padded),
                        rows_per_image: Some(rows),
                    },
                },
                wgpu::Extent3d {
                    width,
                    height: rows,
                    depth_or_array_layers: 1,
                },
            );
            queue.submit(Some(encoder.finish()));

            // Only the rows this band wrote: the buffer is a whole band long and
            // the last one is usually short, so mapping all of it would append
            // whatever the previous band left behind.
            let slice = staging.slice(..(padded as u64) * (rows as u64));
            slice.map_async(wgpu::MapMode::Read, |_| {});
            let _ = device.poll(wgpu::PollType::wait_indefinitely());

            let mapped = slice.get_mapped_range();
            for row in 0..rows {
                let start = (row * padded) as usize;
                out.extend_from_slice(&mapped[start..start + unpadded as usize]);
            }
            drop(mapped);
            staging.unmap();
            first += rows;
        }
        out
    }

    /// Write a previously captured rectangle back into one layer.
    pub fn write_layer_rect(
        &mut self,
        queue: &wgpu::Queue,
        slot: u32,
        rect: PixelRect,
        bytes: &[u8],
    ) {
        debug_assert_eq!(bytes.len() as u64, rect.area() * 4);
        self.touch_slot(slot);
        // As in `read_layer_rect`: refuse rather than abort. See there.
        if slot >= self.layers.capacity {
            log::error!(
                "write to slot {slot} beyond capacity {}",
                self.layers.capacity
            );
            return;
        }
        // The importers promise canvas-sized pixels and the undo stack promises
        // rect-sized ones, but both come from files, and a short buffer here is
        // a validation error rather than a wrong picture.
        if (bytes.len() as u64) < rect.area() * 4 {
            log::error!(
                "layer write has {} bytes for a {}x{} rect",
                bytes.len(),
                rect.width,
                rect.height
            );
            return;
        }
        write_rect(
            queue,
            &self.layers.texture,
            wgpu::Origin3d {
                x: rect.x,
                y: rect.y,
                z: slot,
            },
            rect,
            bytes,
        );
    }
}

/// The smallest rectangle covering both, where either may be absent.
///
/// What a preview has to restore: the rectangle it wrote last time and the one
/// it is about to write. Missing either leaves a trail of the drag behind on
/// the canvas.
fn span(a: Option<PixelRect>, b: Option<PixelRect>) -> Option<PixelRect> {
    match (a, b) {
        (Some(a), Some(b)) => Some(transform::union(a, b)),
        (a, b) => a.or(b),
    }
}

/// Upload tightly packed RGBA8 into a rectangle of a texture.
fn write_rect(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    origin: wgpu::Origin3d,
    rect: PixelRect,
    bytes: &[u8],
) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin,
            aspect: wgpu::TextureAspect::All,
        },
        bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(rect.width * 4),
            rows_per_image: Some(rect.height),
        },
        wgpu::Extent3d {
            width: rect.width,
            height: rect.height,
            depth_or_array_layers: 1,
        },
    );
}

fn mode_index(mode: BrushMode) -> u32 {
    match mode {
        BrushMode::Paint => 0,
        BrushMode::Erase => 1,
    }
}

fn clear_view(encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, label: &str) {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            depth_slice: None,
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
}

/// Upload an 8-bit mask — a tip or a paper tile — into a fresh texture.
fn upload_mask(device: &wgpu::Device, queue: &wgpu::Queue, mask: &TipMask) -> wgpu::Texture {
    upload_coverage(
        device,
        queue,
        mask.width(),
        mask.height(),
        mask.coverage(),
        "umber-brush-tip",
    )
}

/// Put `bytes` — one per texel, row-major — into a new coverage texture.
fn upload_coverage(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    width: u32,
    height: u32,
    bytes: &[u8],
    label: &str,
) -> wgpu::Texture {
    let texture = make_coverage_texture(device, width, height, label);
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            // One byte per texel: R8Unorm is all a coverage mask needs, matching
            // the stroke scratch it feeds.
            bytes_per_row: Some(width),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    texture
}

/// Storage for a brush tip: single-channel coverage, matching the stroke
/// scratch it feeds. Four channels would be four times the bandwidth to say the
/// same thing.
fn make_tip_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    make_coverage_texture(device, width, height, "umber-brush-tip")
}

/// Storage for a coloured stamp's colour. See [`TIP_COLOR_FORMAT`].
fn make_tip_color_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("umber-brush-tip-colour"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TIP_COLOR_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// Upload a coloured stamp's colour plane.
///
/// The premultiply is `umber-core`'s — [`TipMask::colour_premultiplied`] — for
/// the reason every other conversion in this codebase lives there: it is
/// arithmetic with an exact inverse and it is testable without a device. This
/// function is the `write_texture` and nothing else.
fn upload_tip_color(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    mask: &TipMask,
    rgba: &[u8],
) -> wgpu::Texture {
    let texture = make_tip_color_texture(device, mask.width(), mask.height());
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(mask.width() * 4),
            rows_per_image: Some(mask.height()),
        },
        wgpu::Extent3d {
            width: mask.width(),
            height: mask.height(),
            depth_or_array_layers: 1,
        },
    );
    texture
}

/// Single-channel coverage storage — a brush tip, a paper tile or a selection
/// mask. One function because they are the same texture with different
/// contents, and a second copy of this descriptor is a second place for the
/// format to drift from the stroke scratch these all feed.
fn make_coverage_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    label: &str,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: STROKE_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

#[allow(clippy::too_many_arguments)]
fn make_dab_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniforms: &wgpu::Buffer,
    tip: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    grain: &wgpu::TextureView,
    grain_sampler: &wgpu::Sampler,
    selection: &wgpu::TextureView,
    tip_color: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("dab-bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(tip),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(grain),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::Sampler(grain_sampler),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::TextureView(selection),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: wgpu::BindingResource::TextureView(tip_color),
            },
        ],
    })
}

/// The stroke's coverage scratch: one channel, canvas-sized.
fn make_stroke_texture(device: &wgpu::Device, size: UVec2) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("umber-stroke-scratch"),
        size: wgpu::Extent3d {
            width: size.x.max(1),
            height: size.y.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: STROKE_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    })
}

fn make_stroke_color_texture(device: &wgpu::Device, size: UVec2) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("umber-stroke-colour"),
        size: wgpu::Extent3d {
            width: size.x.max(1),
            height: size.y.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: STROKE_COLOR_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    })
}

fn make_composite_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniforms: &wgpu::Buffer,
    layers: &wgpu::TextureView,
    stroke: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    stroke_color: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("composite-bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(layers),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(stroke),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(stroke_color),
            },
        ],
    })
}

fn make_commit_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniforms: &wgpu::Buffer,
    stroke: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    stroke_color: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("commit-bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(stroke),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(stroke_color),
            },
        ],
    })
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn texture_array_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2Array,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `MAX_DRAWS` the shader compiles, as an integer.
    ///
    /// Parsed out of the WGSL text because that is the only way to read it: the
    /// shader is a string until naga sees it, so nothing in Rust can name the
    /// constant. Deliberately strict about the shape of the line — a parse that
    /// quietly failed and answered a default would be a guard that agrees with
    /// whatever it is compared against.
    fn shader_max_draws() -> usize {
        const NEEDLE: &str = "const MAX_DRAWS: u32 = ";
        let at = COMPOSITE_WGSL
            .find(NEEDLE)
            .expect("composite.wgsl no longer declares `const MAX_DRAWS: u32 = ...`");
        let rest = &COMPOSITE_WGSL[at + NEEDLE.len()..];
        let end = rest
            .find("u;")
            .expect("`MAX_DRAWS` is no longer a `u32` literal ending in `u;`");
        rest[..end]
            .trim()
            .parse()
            .expect("`MAX_DRAWS` is not a plain decimal literal")
    }

    /// The shader text alone. **Not** [`COMPOSITE_SHADER`], which has
    /// `blend.wgsl` concatenated in front of it — the needles below want the
    /// one file the constant and the arrays are declared in.
    const COMPOSITE_WGSL: &str = include_str!("../shaders/composite.wgsl");

    /// Every length the shader declares a `vec4<f32>` array at.
    ///
    /// **Reading the constant is not enough**, and this is the gap the first
    /// draft had. `shader_max_draws` proves what `MAX_DRAWS` *is*, not that the
    /// arrays use it: leave the constant at 191 and write
    /// `array<vec4<f32>, 64>` and the WGSL struct is 2160 bytes against a
    /// 6224-byte buffer. That direction **passes validation** — a binding may
    /// be larger than the struct — and the composite then reads `extra` as
    /// `layers` for every draw past the 63rd. The reverse fails loudly, which
    /// is why only this one needed catching.
    fn shader_array_lengths() -> Vec<&'static str> {
        const OPEN: &str = "array<vec4<f32>, ";
        COMPOSITE_WGSL
            .match_indices(OPEN)
            .map(|(at, _)| {
                let rest = &COMPOSITE_WGSL[at + OPEN.len()..];
                let end = rest.find('>').expect("an unterminated array type");
                rest[..end].trim()
            })
            .collect()
    }

    /// **Three numbers have to agree, and this is what says so.**
    ///
    /// `LayerStack::MAX` bounds stack entries; [`MAX_DRAWS`] here and
    /// `MAX_DRAWS` in `composite.wgsl` size the composite pass's two uniform
    /// arrays, and must be the same or the Rust struct and the WGSL one stop
    /// matching byte for byte. There used to be two, all equal; a layer's
    /// effects each composite as a draw of their own, so the stack cap and the
    /// draw cap are now different quantities. A later change to any one of the
    /// three is exactly the kind of thing that breaks in silence.
    #[test]
    fn the_three_draw_capacities_agree() {
        assert_eq!(
            MAX_LAYERS,
            umber_core::LayerStack::MAX,
            "the stack cap here and in umber-core have drifted"
        );
        assert_eq!(
            MAX_DRAWS,
            shader_max_draws(),
            "MAX_DRAWS in canvas.rs and composite.wgsl have drifted"
        );

        // And that the arrays are declared at it, not merely that the constant
        // holds it. See `shader_array_lengths` for the failure this catches.
        let lengths = shader_array_lengths();
        assert_eq!(
            lengths.len(),
            2,
            "composite.wgsl no longer declares exactly two vec4 arrays: {lengths:?}"
        );
        for len in lengths {
            assert_eq!(
                len, "MAX_DRAWS",
                "a uniform array is sized by something other than MAX_DRAWS"
            );
        }
    }

    /// The one number here that is not ours, and the two that come out of it.
    ///
    /// **The derivation is what is tested, at inputs other than the shipped
    /// ones, and it is tested by *calling* it.** Asserting `MAX_SLOTS ==
    /// MAX_LAYERS * 2 + 1 + (MAX_DRAWS - MAX_LAYERS)` is a copy of the formula
    /// and cannot fail when the formula is wrong. Nor is it enough to recompute
    /// the subtraction in the test and check it against itself: `a - b + b ==
    /// a` holds whatever [`effect_slices`] does. Both drafts of this test made
    /// one of those two mistakes. What runs below is the real
    /// [`effect_slices`] and [`draws`], and the claims are about *them*.
    #[test]
    fn the_slice_ceiling_agrees_with_umber_core() {
        assert_eq!(MAX_SLOTS as u32, umber_core::LayerStack::MAX_SLOTS);
        assert_eq!(MAX_LAYERS, umber_core::LayerStack::MAX);

        // The shipped constants really are what the derivation answers, so
        // everything proved about the functions is proved about them.
        assert_eq!(MAX_EFFECT_SLICES, effect_slices(MAX_LAYERS, MAX_SLOTS));
        assert_eq!(MAX_DRAWS - MAX_LAYERS, MAX_EFFECT_SLICES);
        assert_eq!(MAX_EFFECT_SLICES, 127);
        assert_eq!(MAX_DRAWS, 191);

        // The model's budget and this array's capacity are the same quantity
        // spelled in two crates, and **this is the only place both can be
        // seen**: `umber-core` may not depend on wgpu, so it cannot derive its
        // figure from the device's ceiling and carries a literal instead. An
        // effect draw reads an effect slice, one for one, so a model that let
        // more effects be added than there are slices would be promising a
        // draw with nothing to read.
        assert_eq!(
            umber_core::effect::MAX_ENABLED,
            MAX_EFFECT_SLICES,
            "the model's effect budget and the renderer's slice budget are one number",
        );

        // The ceiling is fixed by the device, so it is the one input that does
        // not vary. `MAX_LAYERS` is ours and may move, which is the whole
        // reason these are functions.
        for layers in [1usize, 8, 32, 64, 100, 127] {
            let effects = effect_slices(layers, MAX_SLOTS);
            // One draw per layer and one per effect slice, which is how
            // `MAX_DRAWS` is built.
            let total = layers + effects;

            // Every slice is spoken for exactly once: a layer, a mask, the
            // float's spare, or an effect. Drop the `+ 1` from
            // `effect_slices` and this is the assertion that goes red — and so
            // does a body that is right at 64 and wrong at 1, which is the
            // mutation this loop exists for.
            assert_eq!(layers * 2 + 1 + effects, MAX_SLOTS, "{layers} layers");
            // The draw list can never outrun the array it draws from, which is
            // the promise 192 broke.
            assert!(total <= MAX_SLOTS, "{layers} layers: {total} draws");
        }

        // Raising it allocates nothing: the array starts at `INITIAL_SLOTS` and
        // `ensure_slots` doubles towards what is actually claimed.
        assert!(INITIAL_SLOTS < MAX_SLOTS as u32);
    }

    /// The composite uniform is the one block here large enough for the
    /// question to be worth asking, and raising [`MAX_DRAWS`] is what makes it
    /// so. Both halves are checked: the size the arithmetic in
    /// [`ViewUniforms`]' doc comment claims, and that it clears the smallest
    /// binding a device Umber will run on has to offer.
    #[test]
    fn the_view_uniform_fits_the_smallest_binding_a_device_must_offer() {
        // The head is **measured**, not restated. `112 % 16 == 0` written as a
        // literal is a tautology that cannot fail; what has to be true is that
        // the offset Rust actually gives `layers` is 16-aligned, because WGSL
        // aligns an `array<vec4<f32>>` to 16 and would insert padding there
        // that `#[repr(C)]` does not — leaving the buffer short by however
        // much, and every draw after the gap reading the wrong entry.
        let head = std::mem::offset_of!(ViewUniforms, layers);
        assert_eq!(head, 112, "the scalar head of the block changed size");
        assert_eq!(head % 16, 0, "WGSL would pad where Rust does not");
        assert_eq!(
            std::mem::offset_of!(ViewUniforms, extra),
            head + MAX_DRAWS * 16,
            "the two arrays are not back to back"
        );

        let size = std::mem::size_of::<ViewUniforms>();
        assert_eq!(size, head + MAX_DRAWS * 32);
        assert_eq!(size, 6224, "the figure in the doc comment is stale");
        // The struct's own alignment in WGSL is 16, so its size rounds up to a
        // multiple of it. Rust's is 4, and a mismatch here would be tail
        // padding on one side only.
        assert_eq!(size % 16, 0);

        // `Gpu::new` asks for `downlevel_defaults`, and `using_resolution`
        // raises only the texture dimensions, so this is the limit in force on
        // every adapter Umber accepts.
        let limit = wgpu::Limits::downlevel_defaults().max_uniform_buffer_binding_size as usize;
        assert_eq!(limit, 16 << 10, "downlevel_defaults moved under us");
        assert!(
            size <= limit,
            "the composite uniform is {size} bytes against a {limit}-byte binding limit"
        );
    }

    /// One slice of a square canvas, in bytes.
    fn slice_of(side: u64) -> u64 {
        side * side * LAYER_BYTES_PER_PIXEL
    }

    /// **The regression this policy exists for, stated as the allocation it
    /// makes.**
    ///
    /// 64 layers each with a mask is 128 slices and a legal document with no
    /// effects in it; `begin_float` then asks for the 129th. Under plain
    /// doubling that allocates 256 — 4.29 GB at 2048² against the 2.06 GB
    /// asked for, with the old array still alive during the copy and no
    /// shrinking afterwards.
    ///
    /// It passed for the wrong reason before `MAX_SLOTS` moved: `.min(129)`
    /// clamped the overshoot away, so the doubling was never exercised and the
    /// clamp was load-bearing without saying so. **Nothing measured the
    /// allocated capacity at all** — the only two assertions in the suite are
    /// `< 8` and `>= 8` — which is why raising the ceiling to 256 changed the
    /// behaviour in silence.
    #[test]
    fn a_document_does_not_double_its_layer_array_to_reach_one_more_slice() {
        // 2048², the canvas the arithmetic was worked out on.
        assert_eq!(grown_capacity(128, 129, slice_of(2048)), 129);
        // And at 10000², where the overshoot alone would be 50 GB.
        assert_eq!(grown_capacity(128, 129, slice_of(10_000)), 129);
        // The clamp in `ensure_slots` is not what is doing this: 129 is well
        // under the 256 ceiling, so a reverted policy would return 256 here
        // and the clamp would pass it straight through.
        assert!(129 < MAX_SLOTS as u32);
    }

    /// Growth stays amortised while a slice is cheap, which is the whole reason
    /// doubling is there.
    #[test]
    fn a_small_canvas_still_doubles() {
        // 256², 256 KiB a slice: a handful of slices is nothing, so a document
        // adding its fifth layer gets room for eight.
        assert_eq!(grown_capacity(4, 5, slice_of(256)), 8);
        // And from one, up to the first power of two that holds it.
        assert_eq!(grown_capacity(1, 5, slice_of(256)), 8);
    }

    /// The budget is in bytes, so the same slice count behaves differently on
    /// different canvases — which is the point of stating it that way.
    #[test]
    fn the_growth_budget_is_measured_in_bytes_not_slices() {
        // 16 MiB a slice: doubling to 16 slices is 256 MiB and allowed, to 32
        // is 512 MiB and refused.
        assert_eq!(grown_capacity(8, 9, slice_of(2048)), 16);
        assert_eq!(grown_capacity(16, 17, slice_of(2048)), 17);
        // 400 MB a slice: one slice is already over the budget, so every
        // growth is exact and nothing is allocated on speculation.
        for needed in [2u32, 3, 9, 100] {
            assert_eq!(
                grown_capacity(needed - 1, needed, slice_of(10_000)),
                needed,
                "{needed} slices at 10000²"
            );
        }
    }

    /// Total, and never short of what was asked for. The second is the one that
    /// would be a validation error rather than a waste.
    #[test]
    fn the_growth_rule_always_reaches_what_was_asked_for() {
        for side in [1u64, 64, 256, 2048, 10_000] {
            for needed in 1..=MAX_SLOTS as u32 {
                for current in [0u32, 1, 4, needed.saturating_sub(1)] {
                    let got = grown_capacity(current, needed, slice_of(side));
                    assert!(got >= needed, "{side}²: {current} -> {needed} gave {got}");
                }
            }
        }
        // A degenerate canvas has no bytes to budget against and must still
        // terminate rather than spinning or overflowing.
        assert!(grown_capacity(1, 200, 0) >= 200);
    }

    #[test]
    fn a_band_is_the_whole_document_when_it_fits() {
        // The ordinary case, and the one that must not change: every readback
        // stays a single copy for every document a device can hold in one
        // buffer. 2048² at four bytes is 16 MB against a 256 MB limit.
        assert_eq!(band_rows(256 << 20, 2048 * 4, 2048), 2048);
    }

    #[test]
    fn a_document_larger_than_the_limit_is_split() {
        // The canvas that crashed: 10000², 40 KB a row, against 256 MB.
        let padded = 10_000 * 4;
        let band = band_rows(256 << 20, padded, 10_000);
        assert!(band < 10_000, "should have been split, got {band}");
        assert!(
            u64::from(band) * u64::from(padded) <= 256 << 20,
            "a band of {band} rows is over the limit"
        );
        // And the band after it must still reach the bottom of the document.
        assert!(band * 2 >= 10_000, "{band} rows would need three passes");
    }

    #[test]
    fn a_band_is_never_zero_rows() {
        // A row wider than the whole limit cannot be honoured, and returning
        // zero would be an infinite loop rather than a refusal. It takes a
        // canvas 67 million pixels across, which `max_texture_dimension_2d`
        // stops long before this is asked.
        assert_eq!(band_rows(16, 4096, 100), 1);
        assert_eq!(band_rows(0, 4, 100), 1);
    }

    #[test]
    fn a_band_never_overshoots_the_document() {
        // The copy's extent comes from this; a band taller than what is left
        // would name rows past the bottom of the texture, which is a validation
        // error and therefore an abort.
        assert_eq!(band_rows(u64::MAX, 256, 7), 7);
    }
}
