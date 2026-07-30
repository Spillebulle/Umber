//! The canvas renderer: layer storage, stroke scratch surface and the three
//! pipelines that move data between them.

use bytemuck::{Pod, Zeroable};
use glam::{UVec2, Vec2};
use umber_core::{BrushMode, Camera, Color, Dab, PixelRect};
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

const DAB_STRIDE: u64 = std::mem::size_of::<Dab>() as u64;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct DabUniforms {
    doc_size: [f32; 2],
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ViewUniforms {
    scale: [f32; 2],
    offset: [f32; 2],
    doc_size: [f32; 2],
    viewport: [f32; 2],
    color: [f32; 4],
    mode: u32,
    checker: f32,
    _pad: [f32; 2],
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
    _pad1: f32,
    _pad2: [f32; 2],
}

pub struct CanvasRenderer {
    doc_size: UVec2,

    layer: wgpu::Texture,
    layer_view: wgpu::TextureView,
    // Held for ownership and for the document-resize path; the views and bind
    // groups are what the passes actually touch.
    #[allow(dead_code)]
    stroke: wgpu::Texture,
    stroke_view: wgpu::TextureView,

    dab_pipeline: wgpu::RenderPipeline,
    dab_bind_group: wgpu::BindGroup,
    #[allow(dead_code)]
    dab_uniforms: wgpu::Buffer,
    dab_instances: wgpu::Buffer,
    /// Dabs already written into `dab_instances` this frame.
    dabs_this_frame: u32,

    composite_pipeline: wgpu::RenderPipeline,
    composite_bind_group: wgpu::BindGroup,
    view_uniforms: wgpu::Buffer,

    commit_pipeline: wgpu::RenderPipeline,
    commit_erase_pipeline: wgpu::RenderPipeline,
    commit_bind_group: wgpu::BindGroup,
    commit_uniforms: wgpu::Buffer,
}

impl CanvasRenderer {
    pub fn new(
        device: &wgpu::Device,
        doc_size: UVec2,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        let (layer, layer_view) = create_texture(
            device,
            "umber-layer",
            doc_size,
            LAYER_FORMAT,
            wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
        );
        let (stroke, stroke_view) = create_texture(
            device,
            "umber-stroke-scratch",
            doc_size,
            STROKE_FORMAT,
            wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
        );

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
        let dab_uniforms = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("dab-uniforms"),
            contents: bytemuck::bytes_of(&DabUniforms {
                doc_size: [doc_size.x as f32, doc_size.y as f32],
                _pad: [0.0; 2],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let dab_instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dab-instances"),
            size: DAB_STRIDE * MAX_DABS_PER_FRAME as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let dab_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dab-bgl"),
            entries: &[uniform_entry(0)],
        });
        let dab_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dab-bg"),
            layout: &dab_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: dab_uniforms.as_entire_binding(),
            }],
        });

        let dab_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("dab-pipeline"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("dab-pl"),
                    bind_group_layouts: &[Some(&dab_layout)],
                    immediate_size: 0,
                }),
            ),
            vertex: wgpu::VertexState {
                module: &dab_shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: DAB_STRIDE,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
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
                    ],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &dab_shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: STROKE_FORMAT,
                    // `Max` is the whole trick: coverage saturates instead of
                    // accumulating, so a stroke crossing itself stays even.
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

        // ---- composite pass -------------------------------------------------
        let composite_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("composite"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/composite.wgsl").into()),
        });
        let view_uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("view-uniforms"),
            size: std::mem::size_of::<ViewUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("composite-bgl"),
            entries: &[
                uniform_entry(0),
                texture_entry(1),
                texture_entry(2),
                sampler_entry(3),
            ],
        });
        let composite_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("composite-bg"),
            layout: &composite_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: view_uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&layer_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&stroke_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let composite_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("composite-pipeline"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("composite-pl"),
                    bind_group_layouts: &[Some(&composite_layout)],
                    immediate_size: 0,
                }),
            ),
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
                targets: &[Some(surface_format.into())],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // ---- commit pass ----------------------------------------------------
        let commit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("commit"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/commit.wgsl").into()),
        });
        let commit_uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("commit-uniforms"),
            size: std::mem::size_of::<CommitUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let commit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("commit-bgl"),
            entries: &[uniform_entry(0), texture_entry(1), sampler_entry(2)],
        });
        let commit_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("commit-bg"),
            layout: &commit_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: commit_uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&stroke_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
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
            doc_size,
            layer,
            layer_view,
            stroke,
            stroke_view,
            dab_pipeline,
            dab_bind_group,
            dab_uniforms,
            dab_instances,
            dabs_this_frame: 0,
            composite_pipeline,
            composite_bind_group,
            view_uniforms,
            commit_pipeline,
            commit_erase_pipeline,
            commit_bind_group,
            commit_uniforms,
        }
    }

    pub fn doc_size(&self) -> UVec2 {
        self.doc_size
    }

    /// Reset the per-frame instance cursor. Call once at the top of a frame.
    pub fn begin_frame(&mut self) {
        self.dabs_this_frame = 0;
    }

    /// Upload dabs and stamp them into the scratch texture.
    ///
    /// Safe to call with an empty slice; does nothing in that case.
    pub fn draw_dabs(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        dabs: &[Dab],
    ) {
        if dabs.is_empty() {
            return;
        }
        let room = MAX_DABS_PER_FRAME.saturating_sub(self.dabs_this_frame as usize);
        if room == 0 {
            log::warn!("dab instance buffer full, dropping {} dabs", dabs.len());
            return;
        }
        let dabs = &dabs[..dabs.len().min(room)];

        let offset = self.dabs_this_frame as u64 * DAB_STRIDE;
        queue.write_buffer(&self.dab_instances, offset, bytemuck::cast_slice(dabs));

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("dab-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.stroke_view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    // Load: the scratch accumulates across frames for the whole
                    // stroke, so only the new dabs are drawn each frame.
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.dab_pipeline);
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

    /// Draw layer + in-progress stroke to the given target.
    #[allow(clippy::too_many_arguments)]
    pub fn composite(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        camera: &Camera,
        viewport: Vec2,
        color: Color,
        opacity: f32,
        mode: BrushMode,
    ) {
        let scale = 1.0 / camera.zoom;
        let offset = camera.center - viewport * 0.5 * scale;

        queue.write_buffer(
            &self.view_uniforms,
            0,
            bytemuck::bytes_of(&ViewUniforms {
                scale: [scale, scale],
                offset: [offset.x, offset.y],
                doc_size: [self.doc_size.x as f32, self.doc_size.y as f32],
                viewport: [viewport.x, viewport.y],
                color: [color.r, color.g, color.b, opacity.clamp(0.0, 1.0)],
                mode: mode_index(mode),
                checker: 8.0,
                _pad: [0.0; 2],
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
        pass.set_pipeline(&self.composite_pipeline);
        pass.set_bind_group(0, &self.composite_bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Bake the scratch stroke into the layer over `rect`, then clear scratch.
    pub fn commit_stroke(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        rect: PixelRect,
        color: Color,
        opacity: f32,
        mode: BrushMode,
    ) {
        queue.write_buffer(
            &self.commit_uniforms,
            0,
            bytemuck::bytes_of(&CommitUniforms {
                rect_min: [rect.x as f32, rect.y as f32],
                rect_max: [(rect.x + rect.width) as f32, (rect.y + rect.height) as f32],
                doc_size: [self.doc_size.x as f32, self.doc_size.y as f32],
                _pad0: [0.0; 2],
                color: [color.r, color.g, color.b, opacity.clamp(0.0, 1.0)],
                mode: mode_index(mode),
                _pad1: 0.0,
                _pad2: [0.0; 2],
            }),
        );

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("commit-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.layer_view,
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
            pass.set_pipeline(match mode {
                BrushMode::Paint => &self.commit_pipeline,
                BrushMode::Erase => &self.commit_erase_pipeline,
            });
            pass.set_bind_group(0, &self.commit_bind_group, &[]);
            pass.draw(0..4, 0..1);
        }

        self.clear_stroke(encoder);
    }

    /// Wipe the scratch surface.
    pub fn clear_stroke(&self, encoder: &mut wgpu::CommandEncoder) {
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear-stroke"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.stroke_view,
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

    pub fn clear_layer(&self, encoder: &mut wgpu::CommandEncoder) {
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear-layer"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &self.layer_view,
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

    /// Read a rectangle of the layer back to the CPU, for the undo stack.
    ///
    /// This blocks until the GPU catches up. That is acceptable because it runs
    /// once per stroke at pointer-up, never inside the drawing loop.
    pub fn read_layer_rect(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rect: PixelRect,
    ) -> Vec<u8> {
        let unpadded = rect.width * 4;
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
                texture: &self.layer,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: rect.x,
                    y: rect.y,
                    z: 0,
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

    /// Write a previously captured rectangle back into the layer.
    pub fn write_layer_rect(&self, queue: &wgpu::Queue, rect: PixelRect, bytes: &[u8]) {
        debug_assert_eq!(bytes.len() as u64, rect.area() * 4);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.layer,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: rect.x,
                    y: rect.y,
                    z: 0,
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

fn create_texture(
    device: &wgpu::Device,
    label: &str,
    size: UVec2,
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size.x,
            height: size.y,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
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

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}
