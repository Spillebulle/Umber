//! The canvas renderer: layer storage, stroke scratch surface and the three
//! pipelines that move data between them.

use bytemuck::{Pod, Zeroable};
use glam::{UVec2, Vec2};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use umber_core::{
    Anchor, Background, BrushMode, Camera, CanvasCopy, Color, Dab, PixelRect, TipMask,
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
/// The stroke scratch only needs coverage, so one channel instead of four —
/// a 4x saving on the bandwidth of the hottest texture in the frame.
const STROKE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;

/// Instance buffer capacity, in dabs. A single frame only ever holds the dabs
/// generated since the last frame; 64k is far more than a 120 Hz frame of
/// even the fastest flick can produce.
const MAX_DABS_PER_FRAME: usize = 65_536;

/// Mirrored by `MAX_LAYERS` in `composite.wgsl` and `LayerStack::MAX` in
/// umber-core. All three must agree.
const MAX_LAYERS: usize = 64;

/// Texture-array slices allocated up front. Growth doubles this, so a typical
/// document never pays for a copy.
const INITIAL_SLOTS: u32 = 4;

const DAB_STRIDE: u64 = std::mem::size_of::<Dab>() as u64;

/// Per-dab colour, for a smudging stroke only.
///
/// `Rgba16Float` rather than `Rgba8Unorm` because these are **linear** values.
/// Eight bits of linear light bands visibly in the shadows, and a blender
/// working over a dark painting is precisely where it would show. Allocated
/// only when a smudging stroke starts, so an ordinary session never holds it.
const STROKE_COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

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
/// than about `1/255` rounds away and a stroke of them never builds. Real
/// texture stamps are nowhere near that faint — the sparsest pack measured runs
/// to a peak of 0.49 — and widening the hottest texture in the frame to sixteen
/// bits to hold a coverage nobody can see would be the wrong trade.
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

    /// Take the next slice of rows out of the mapped buffer. Returns true once
    /// the whole step is out of it.
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
        let buffer = self.buffer.as_ref().expect("a mapped step has its buffer");
        let mapped = buffer.slice(..).get_mapped_range();

        let out = self
            .partial
            .get_or_insert_with(|| Vec::with_capacity(row * height));
        let from = out.len() / row;
        let rows = (CAPTURE_CHUNK_BYTES / self.padded as usize).max(1);
        let to = (from + rows).min(height);
        for y in from..to {
            let start = y * self.padded as usize;
            out.extend_from_slice(&mapped[start..start + row]);
        }
        to >= height
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

/// One layer's contribution to the composite, in stack order.
#[derive(Clone, Copy, Debug)]
pub struct LayerDraw {
    /// Texture-array slice holding the pixels.
    pub slot: u32,
    pub opacity: f32,
    /// Matches `umber_core::BlendMode::index`.
    pub blend: u32,
    pub visible: bool,
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
    /// The stroke deposits a colour per dab — it smudges — so `color` is only
    /// the fallback and the real colour comes from the stroke's colour scratch.
    ///
    /// Must match what was passed to [`CanvasRenderer::draw_dabs`] for the same
    /// stroke. Preview and commit both read it, which is what keeps them from
    /// disagreeing about where the colour came from.
    pub per_dab_color: bool,
}

impl Default for StrokeStyle {
    fn default() -> Self {
        Self {
            color: Color::BLACK,
            opacity: 1.0,
            mode: BrushMode::Paint,
            per_dab_color: false,
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
}

impl DabUniforms {
    /// The uniforms for a document with no tip and no grain: every factor the
    /// shader multiplies by is one.
    fn plain(doc_size: UVec2) -> Self {
        Self {
            doc_size: [doc_size.x as f32, doc_size.y as f32],
            tip_scale: [1.0, 1.0],
            use_tip: 0,
            grain_strength: 0.0,
            grain_scale: 1.0,
            _pad: 0.0,
        }
    }
}

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
    _pad: [u32; 2],
    /// (opacity, blend, slot, visible) per stack position.
    layers: [[f32; 4]; MAX_LAYERS],
}

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
    _pad2: [f32; 2],
}

/// The layer texture array and the views onto it.
struct LayerStore {
    texture: wgpu::Texture,
    /// Sampled by the composite pass.
    array_view: wgpu::TextureView,
    /// One per slice, used as render targets by commit and clear.
    slot_views: Vec<wgpu::TextureView>,
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
            view_formats: &[],
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

        Self {
            texture,
            array_view,
            slot_views,
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

    dab_bind_group: wgpu::BindGroup,
    dab_uniforms: wgpu::Buffer,
    dab_instances: wgpu::Buffer,
    dabs_this_frame: u32,
    /// The bitmap tip, or a 1x1 placeholder. Held so it outlives the bind
    /// group that references it.
    tip: wgpu::Texture,
    has_tip: bool,
    /// Which mask is in that texture, so [`CanvasRenderer::set_tip`] can tell
    /// "the same brush again" from "a different brush".
    tip_mask: Option<Arc<TipMask>>,
    /// The paper tile, or a 1x1 placeholder. Held so it outlives the bind group.
    grain: wgpu::Texture,
    grain_view: wgpu::TextureView,
    /// Which tile is in that texture, compared by `Arc` identity for exactly
    /// the reason [`CanvasRenderer::tip_mask`] is.
    grain_tile: Option<Arc<TipMask>>,
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
        let composite_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("composite"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/composite.wgsl").into()),
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
        let commit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("commit"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/commit.wgsl").into()),
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
        let dab_bind_group = make_dab_bind_group(
            device,
            &shared.dab_layout,
            &dab_uniforms,
            &tip_view,
            &shared.sampler,
            &grain_view,
            &shared.grain_sampler,
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
            dab_bind_group,
            dab_uniforms,
            dab_instances,
            dabs_this_frame: 0,
            tip,
            has_tip: false,
            tip_mask: None,
            grain,
            grain_view,
            grain_tile: None,
            grain_params: (0.0, 1.0),
            dab_state: DabUniforms::plain(doc_size),
            composite_bind_group,
            view_uniforms,
            commit_bind_group,
            commit_uniforms,
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
    /// layers pays for two copies, not eight.
    pub fn ensure_slots(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, needed: u32) {
        if needed <= self.layers.capacity {
            return;
        }
        let mut capacity = self.layers.capacity.max(1);
        while capacity < needed {
            capacity *= 2;
        }
        let capacity = capacity.min(MAX_LAYERS as u32);
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

        // A sample recorded against the old canvas would be read back as if it
        // belonged to the new one.
        self.reset_probes();
        // And a capture half-read against the old canvas would be assembled
        // into a file with layers of two different sizes in it.
        self.cancel_capture();

        self.doc_size = new_size;
        self.dab_state.doc_size = [new_size.x as f32, new_size.y as f32];
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
    pub fn set_tip(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tip: Option<Arc<TipMask>>,
    ) {
        let unchanged = match (&self.tip_mask, &tip) {
            (Some(current), Some(next)) => Arc::ptr_eq(current, next),
            (None, None) => true,
            _ => false,
        };
        if unchanged {
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

        self.tip = texture;
        self.has_tip = has_tip;
        self.tip_mask = tip;
        self.rebuild_dab_bind_group(device);
        queue.write_buffer(&self.dab_uniforms, 0, bytemuck::bytes_of(&self.dab_state));
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

        let mut packed = [[0.0f32; 4]; MAX_LAYERS];
        let count = params.layers.len().min(MAX_LAYERS);
        for (dst, src) in packed.iter_mut().zip(&params.layers[..count]) {
            *dst = [
                src.opacity.clamp(0.0, 1.0),
                src.blend as f32,
                src.slot as f32,
                if src.visible { 1.0 } else { 0.0 },
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
                _pad: [0; 2],
                layers: packed,
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
    pub fn commit_stroke(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        slot: u32,
        rect: PixelRect,
        style: StrokeStyle,
    ) {
        let Some(view) = self.layers.slot_views.get(slot as usize) else {
            log::error!("commit to slot {slot} beyond capacity");
            return;
        };

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
                _pad2: [0.0; 2],
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
            pass.draw(0..4, 0..1);
        }

        self.clear_stroke(encoder);
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
    pub fn clear_layer(&self, encoder: &mut wgpu::CommandEncoder, slot: u32) {
        if let Some(view) = self.layers.slot_views.get(slot as usize) {
            clear_view(encoder, view, "clear-layer");
        }
    }

    /// Wipe every allocated slot. Used at startup.
    pub fn clear_all_layers(&self, encoder: &mut wgpu::CommandEncoder) {
        for view in &self.layers.slot_views {
            clear_view(encoder, view, "clear-layer");
        }
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

        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = (w * 4).div_ceil(align) * align;
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("export-readback"),
            size: (padded * h) as u64,
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
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));

        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = device.poll(wgpu::PollType::wait_indefinitely());

        let mapped = slice.get_mapped_range();
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h {
            let start = (row * padded) as usize;
            out.extend_from_slice(&mapped[start..start + (w * 4) as usize]);
        }
        drop(mapped);
        staging.unmap();
        out
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
        let height = job.size.y;
        // Allocated once and reused for every step. A buffer per layer would be
        // the document's own size in staging memory on top of the copy of it
        // being assembled.
        let buffer = job.buffer.take().unwrap_or_else(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("umber-capture"),
                size: (job.padded * height) as u64,
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
                    y: 0,
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
            let target = self.export_target(device);
            let view = target.create_view(&wgpu::TextureViewDescriptor::default());
            self.render_export(queue, encoder, &view, &job.draws);
            job.merged_target = Some(target);
            wgpu::TexelCopyTextureInfo {
                // Held in `job.merged_target` for the rest of this function.
                texture: job.merged_target.as_ref().expect("just set"),
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
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
        // The preview's offscreen target has served its purpose the moment the
        // copy out of it is submitted; holding it would keep a canvas-sized
        // texture alive for the rest of the readback.
        job.merged_target = None;
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
                    let done = if job.abandoned || job.failed {
                        job.partial = None;
                        true
                    } else {
                        job.copy_chunk()
                    };
                    if done {
                        job.buffer
                            .as_ref()
                            .expect("a mapped step has its buffer")
                            .unmap();
                        if let Some(bytes) = job.partial.take() {
                            job.results[job.step] = Some(bytes);
                            job.step += 1;
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
        let unpadded = rect.width * 4;
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
            return vec![0; (unpadded as u64 * rect.height as u64) as usize];
        }
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;

        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("undo-readback"),
            size: (padded * rect.height) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("undo"),
        });
        encoder.copy_texture_to_buffer(
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
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(rect.height),
                },
            },
            wgpu::Extent3d {
                width: rect.width,
                height: rect.height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));

        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = device.poll(wgpu::PollType::wait_indefinitely());

        let mapped = slice.get_mapped_range();
        // Strip the 256-byte row padding the copy required.
        let mut out = Vec::with_capacity((unpadded * rect.height) as usize);
        for row in 0..rect.height {
            let start = (row * padded) as usize;
            out.extend_from_slice(&mapped[start..start + unpadded as usize]);
        }
        drop(mapped);
        staging.unmap();
        out
    }

    /// Write a previously captured rectangle back into one layer.
    pub fn write_layer_rect(&self, queue: &wgpu::Queue, slot: u32, rect: PixelRect, bytes: &[u8]) {
        debug_assert_eq!(bytes.len() as u64, rect.area() * 4);
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
        queue.write_texture(
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
    let texture = make_tip_texture(device, mask.width(), mask.height());
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        mask.coverage(),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            // One byte per texel: R8Unorm is all a coverage mask needs, matching
            // the stroke scratch it feeds.
            bytes_per_row: Some(mask.width()),
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

/// Storage for a brush tip: single-channel coverage, matching the stroke
/// scratch it feeds. Four channels would be four times the bandwidth to say the
/// same thing.
fn make_tip_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("umber-brush-tip"),
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
