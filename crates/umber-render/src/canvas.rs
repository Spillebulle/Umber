//! The canvas renderer: layer storage, stroke scratch surface and the three
//! pipelines that move data between them.

use bytemuck::{Pod, Zeroable};
use glam::{UVec2, Vec2};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use umber_core::{BrushMode, Camera, Color, Dab, PixelRect, TipMask};
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
    _pad: f32,
}

impl DabUniforms {
    /// The uniforms for a document with no tip bound.
    fn plain(doc_size: UVec2) -> Self {
        Self {
            doc_size: [doc_size.x as f32, doc_size.y as f32],
            tip_scale: [1.0, 1.0],
            use_tip: 0,
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

        // ---- dab pass -------------------------------------------------------
        let dab_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dab"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/dab.wgsl").into()),
        });
        let dab_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dab-bgl"),
            entries: &[uniform_entry(0), texture_entry(1), sampler_entry(2)],
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
    /// does after [`CanvasRenderer::new`].
    pub fn for_document(&self, device: &wgpu::Device, doc_size: UVec2) -> Self {
        Self::with_shared(device, doc_size, self.shared.clone())
    }

    fn with_shared(device: &wgpu::Device, doc_size: UVec2, shared: Shared) -> Self {
        let layers = LayerStore::new(device, doc_size, INITIAL_SLOTS);

        let stroke = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("umber-stroke-scratch"),
            size: wgpu::Extent3d {
                width: doc_size.x,
                height: doc_size.y,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: STROKE_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
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
        let dab_bind_group = make_dab_bind_group(
            device,
            &shared.dab_layout,
            &dab_uniforms,
            &tip_view,
            &shared.sampler,
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
            dab_bind_group,
            dab_uniforms,
            dab_instances,
            dabs_this_frame: 0,
            tip,
            has_tip: false,
            tip_mask: None,
            composite_bind_group,
            view_uniforms,
            commit_bind_group,
            commit_uniforms,
        }
    }

    pub fn doc_size(&self) -> UVec2 {
        self.doc_size
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

        let mut uniforms = DabUniforms::plain(self.doc_size);
        let (texture, has_tip) = match &tip {
            Some(mask) => {
                // The mask's own proportions. Padding it into a square would
                // reach the same geometry and pay for an empty margin in
                // texture memory and in fragments — see `TipMask::aspect`.
                let (sx, sy) = mask.aspect();
                uniforms.tip_scale = [sx, sy];
                uniforms.use_tip = 1;
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
                        // One byte per texel: R8Unorm is all a coverage mask
                        // needs, matching the stroke scratch it feeds.
                        bytes_per_row: Some(mask.width()),
                        rows_per_image: Some(mask.height()),
                    },
                    wgpu::Extent3d {
                        width: mask.width(),
                        height: mask.height(),
                        depth_or_array_layers: 1,
                    },
                );
                (texture, true)
            }
            None => (make_tip_texture(device, 1, 1), false),
        };

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.dab_bind_group = make_dab_bind_group(
            device,
            &self.shared.dab_layout,
            &self.dab_uniforms,
            &view,
            &self.shared.sampler,
        );
        self.tip = texture;
        self.has_tip = has_tip;
        self.tip_mask = tip;

        queue.write_buffer(&self.dab_uniforms, 0, bytemuck::bytes_of(&uniforms));
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

    /// Flatten the visible stack to straight-alpha sRGB bytes, document-sized.
    ///
    /// Runs the same composite pass the screen uses, with its export flag set,
    /// so what lands in the file is what the canvas showed. A separate export
    /// path would be a second copy of the blend maths to keep in step.
    pub fn export_rgba(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layers: &[LayerDraw],
    ) -> Vec<u8> {
        let (w, h) = (self.doc_size.x, self.doc_size.y);

        // Non-sRGB: the shader does its own gamma encode.
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("umber-export"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
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

        // Zoom 1 with the pivot at the document centre makes screen and
        // document coordinates identical, so the render is 1:1.
        let camera = Camera {
            center: Vec2::new(w as f32 * 0.5, h as f32 * 0.5),
            zoom: 1.0,
        };

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("export"),
        });
        self.composite(
            queue,
            &mut encoder,
            &view,
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

    /// Read a rectangle of one layer back to the CPU, for the undo stack.
    ///
    /// This blocks until the GPU catches up. That is acceptable because it runs
    /// once per stroke at pointer-up, never inside the drawing loop.
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

fn make_dab_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniforms: &wgpu::Buffer,
    tip: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
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
        ],
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
