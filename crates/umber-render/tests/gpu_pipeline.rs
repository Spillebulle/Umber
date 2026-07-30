//! End-to-end GPU tests: dab stamping, blending, stroke commit and the layer
//! stack composite.
//!
//! These run headless (no surface) so they work in CI on any machine with a
//! working adapter, including software rasterisers like lavapipe. If no
//! adapter is available at all the tests skip rather than fail, so a machine
//! without a GPU doesn't produce noise.

use glam::{UVec2, Vec2};
use umber_core::{BlendMode, BrushMode, Camera, Color, Dab, PixelRect};
use umber_render::{CanvasRenderer, CompositeParams, Gpu, LayerDraw, StrokeStyle};

const DOC: u32 = 64;

/// Must match the real surface: non-sRGB, because `composite.wgsl` does the
/// gamma encode itself. Using an sRGB target here would encode twice and every
/// expected colour below would be wrong.
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

struct Harness {
    gpu: Gpu,
    canvas: CanvasRenderer,
}

impl Harness {
    fn new() -> Option<Self> {
        let instance = Gpu::create_instance();
        let gpu = pollster::block_on(Gpu::new(instance, None)).ok()?;
        let canvas = CanvasRenderer::new(&gpu.device, UVec2::new(DOC, DOC), TARGET_FORMAT);

        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        canvas.clear_all_layers(&mut enc);
        canvas.clear_stroke(&mut enc);
        gpu.queue.submit(Some(enc.finish()));

        Some(Self { gpu, canvas })
    }

    fn encoder(&self) -> wgpu::CommandEncoder {
        self.gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None })
    }

    fn stamp(&mut self, dabs: &[Dab]) {
        let mut enc = self.encoder();
        self.canvas.begin_frame();
        self.canvas.draw_dabs(&self.gpu.queue, &mut enc, dabs);
        self.gpu.queue.submit(Some(enc.finish()));
    }

    fn commit_to(&mut self, slot: u32, color: Color, opacity: f32, mode: BrushMode) {
        let rect = PixelRect {
            x: 0,
            y: 0,
            width: DOC,
            height: DOC,
        };
        let mut enc = self.encoder();
        self.canvas.commit_stroke(
            &self.gpu.queue,
            &mut enc,
            slot,
            rect,
            StrokeStyle {
                color,
                opacity,
                mode,
            },
        );
        self.gpu.queue.submit(Some(enc.finish()));
    }

    fn commit(&mut self, color: Color, opacity: f32, mode: BrushMode) {
        self.commit_to(0, color, opacity, mode);
    }

    /// Paint a slot solid with `color` around the sample point.
    fn fill(&mut self, slot: u32, color: Color) {
        self.stamp(&[dab(32.0, 32.0, 48.0, 1.0)]);
        self.commit_to(slot, color, 1.0, BrushMode::Paint);
    }

    /// RGBA of a single pixel in a layer's stored bytes.
    fn pixel_in(&self, slot: u32, x: u32, y: u32) -> [u8; 4] {
        let bytes = self.canvas.read_layer_rect(
            &self.gpu.device,
            &self.gpu.queue,
            slot,
            PixelRect {
                x,
                y,
                width: 1,
                height: 1,
            },
        );
        [bytes[0], bytes[1], bytes[2], bytes[3]]
    }

    fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        self.pixel_in(0, x, y)
    }

    /// Run the real composite pass into an offscreen target and read a pixel.
    ///
    /// This is what the user actually sees, so it is the only way to test
    /// per-layer opacity and blend modes.
    fn composite_pixel(&self, layers: &[LayerDraw], x: u32, y: u32) -> [u8; 4] {
        let target = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("test-target"),
            size: wgpu::Extent3d {
                width: DOC,
                height: DOC,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());

        // Viewport equal to the document at zoom 1 makes screen and document
        // coordinates identical, so `x, y` mean what they look like.
        let camera = Camera {
            center: Vec2::splat(DOC as f32 * 0.5),
            zoom: 1.0,
        };

        let mut enc = self.encoder();
        self.canvas.composite(
            &self.gpu.queue,
            &mut enc,
            &view,
            &CompositeParams {
                camera: &camera,
                viewport: Vec2::splat(DOC as f32),
                layers,
                // No stroke in flight for these tests; zero opacity keeps the
                // scratch surface out of the result whatever it contains.
                active_index: 0,
                stroke: StrokeStyle {
                    opacity: 0.0,
                    ..Default::default()
                },
            },
        );

        // 64 px * 4 bytes = 256, already the required copy alignment.
        let row = DOC * 4;
        let staging = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("test-readback"),
            size: (row * DOC) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        enc.copy_texture_to_buffer(
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
                    bytes_per_row: Some(row),
                    rows_per_image: Some(DOC),
                },
            },
            wgpu::Extent3d {
                width: DOC,
                height: DOC,
                depth_or_array_layers: 1,
            },
        );
        self.gpu.queue.submit(Some(enc.finish()));

        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.gpu.device.poll(wgpu::PollType::wait_indefinitely());
        let mapped = slice.get_mapped_range();
        let i = (y * row + x * 4) as usize;
        let px = [mapped[i], mapped[i + 1], mapped[i + 2], mapped[i + 3]];
        drop(mapped);
        staging.unmap();
        px
    }
}

fn dab(x: f32, y: f32, radius: f32, coverage: f32) -> Dab {
    Dab {
        pos: [x, y],
        radius,
        hardness: 0.95,
        coverage,
        _pad: [0.0; 3],
    }
}

fn layer(slot: u32, opacity: f32, blend: BlendMode) -> LayerDraw {
    LayerDraw {
        slot,
        opacity,
        blend: blend.index(),
        visible: true,
    }
}

fn assert_near(actual: [u8; 4], expected: [u8; 3], tolerance: u8, what: &str) {
    let ok = (0..3).all(|i| actual[i].abs_diff(expected[i]) <= tolerance);
    assert!(ok, "{what}: expected ~{expected:?}, got {actual:?}");
}

macro_rules! harness_or_skip {
    () => {
        match Harness::new() {
            Some(h) => h,
            None => {
                eprintln!("no GPU adapter available; skipping");
                return;
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Dab stamping and commit
// ---------------------------------------------------------------------------

#[test]
fn a_committed_dab_marks_the_layer() {
    let mut h = harness_or_skip!();

    assert_eq!(h.pixel(32, 32)[3], 0, "layer should start transparent");

    h.stamp(&[dab(32.0, 32.0, 12.0, 1.0)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);

    assert_eq!(h.pixel(32, 32)[3], 255, "dab centre should be opaque");
    assert_eq!(h.pixel(2, 2)[3], 0, "far corner should be untouched");
}

#[test]
fn overlapping_dabs_do_not_compound() {
    // The core invariant of the wet-layer design: stamping the same
    // half-coverage dab twice must not darken toward 0.75. If this fails,
    // the dab pass has lost its `max` blend and strokes will come out
    // blotchy wherever they overlap or cross themselves.
    let mut h = harness_or_skip!();

    h.stamp(&[dab(32.0, 32.0, 12.0, 0.5), dab(32.0, 32.0, 12.0, 0.5)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);

    let alpha = h.pixel(32, 32)[3];
    assert!(
        (100..=155).contains(&alpha),
        "expected ~128 (single coverage), got {alpha} — dabs are compounding"
    );
}

#[test]
fn stroke_opacity_is_applied_once() {
    let mut h = harness_or_skip!();

    h.stamp(&[dab(32.0, 32.0, 12.0, 1.0)]);
    h.commit(Color::WHITE, 0.5, BrushMode::Paint);

    let alpha = h.pixel(32, 32)[3];
    assert!(
        (100..=155).contains(&alpha),
        "expected ~128 for 50% opacity, got {alpha}"
    );
}

#[test]
fn erasing_removes_coverage() {
    let mut h = harness_or_skip!();

    h.stamp(&[dab(32.0, 32.0, 14.0, 1.0)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);
    assert_eq!(h.pixel(32, 32)[3], 255);

    h.stamp(&[dab(32.0, 32.0, 14.0, 1.0)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Erase);
    assert_eq!(h.pixel(32, 32)[3], 0, "erase should clear alpha");
}

#[test]
fn commit_clears_the_scratch_surface() {
    // If the scratch were left dirty, the next stroke would re-deposit the
    // previous one's coverage the moment it committed.
    let mut h = harness_or_skip!();

    h.stamp(&[dab(20.0, 20.0, 8.0, 1.0)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);
    assert_eq!(h.pixel(20, 20)[3], 255);

    h.commit(Color::WHITE, 1.0, BrushMode::Erase);
    assert_eq!(
        h.pixel(20, 20)[3],
        255,
        "an empty commit must be a no-op, but the scratch still held coverage"
    );
}

#[test]
fn a_dark_colour_survives_the_round_trip_to_the_layer() {
    // The live preview computes in float, but the committed result has to
    // survive 8 bits of layer storage. Storing *linear* values in 8 bits spends
    // almost all its precision on highlights: the default brush colour is
    // sRGB 20, which is linear 0.0056, or 1.4/255. An sRGB-encoded layer
    // distributes precision perceptually instead.
    let mut h = harness_or_skip!();

    h.stamp(&[dab(32.0, 32.0, 12.0, 1.0)]);
    h.commit(Color::from_srgb_u8(20, 20, 24, 255), 1.0, BrushMode::Paint);

    let px = h.pixel(32, 32);
    assert_eq!(px[3], 255, "should be fully opaque");
    assert_near(px, [20, 20, 24], 2, "committed dark ink");
}

// ---------------------------------------------------------------------------
// Undo storage
// ---------------------------------------------------------------------------

#[test]
fn layer_readback_and_writeback_round_trip() {
    let mut h = harness_or_skip!();

    h.stamp(&[dab(32.0, 32.0, 16.0, 1.0)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);

    let rect = PixelRect {
        x: 16,
        y: 16,
        width: 32,
        height: 32,
    };
    let saved = h
        .canvas
        .read_layer_rect(&h.gpu.device, &h.gpu.queue, 0, rect);
    assert_eq!(saved.len(), (rect.width * rect.height * 4) as usize);

    let mut enc = h.encoder();
    h.canvas.clear_layer(&mut enc, 0);
    h.gpu.queue.submit(Some(enc.finish()));
    assert_eq!(h.pixel(32, 32)[3], 0);

    h.canvas.write_layer_rect(&h.gpu.queue, 0, rect, &saved);
    assert_eq!(h.pixel(32, 32)[3], 255, "undo restore lost the pixels");
}

#[test]
fn readback_handles_row_padding() {
    // A 3px-wide rect is 12 bytes per row, far under the 256-byte copy
    // alignment, so this exercises the unpadding path.
    let mut h = harness_or_skip!();

    h.stamp(&[dab(32.0, 32.0, 16.0, 1.0)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);

    let rect = PixelRect {
        x: 31,
        y: 30,
        width: 3,
        height: 5,
    };
    let bytes = h
        .canvas
        .read_layer_rect(&h.gpu.device, &h.gpu.queue, 0, rect);
    assert_eq!(bytes.len(), 3 * 5 * 4);
    assert!(
        bytes.chunks(4).all(|p| p[3] == 255),
        "every pixel inside the dab should be opaque: {bytes:?}"
    );
}

// ---------------------------------------------------------------------------
// Layers
// ---------------------------------------------------------------------------

#[test]
fn layers_do_not_bleed_into_each_other() {
    let mut h = harness_or_skip!();

    h.fill(1, Color::WHITE);

    assert_eq!(h.pixel_in(1, 32, 32)[3], 255, "slot 1 should be painted");
    assert_eq!(h.pixel_in(0, 32, 32)[3], 0, "slot 0 must be untouched");
    assert_eq!(h.pixel_in(2, 32, 32)[3], 0, "slot 2 must be untouched");
}

#[test]
fn growing_the_layer_array_preserves_existing_pixels() {
    // Growth reallocates the texture array and copies every slice. Losing
    // artwork when the user adds a fifth layer would be unforgivable.
    let mut h = harness_or_skip!();

    h.fill(0, Color::from_srgb_u8(200, 40, 40, 255));
    assert!(h.canvas.slot_capacity() < 8);

    h.canvas.ensure_slots(&h.gpu.device, &h.gpu.queue, 8);
    assert!(h.canvas.slot_capacity() >= 8);

    assert_near(h.pixel_in(0, 32, 32), [200, 40, 40], 2, "after growth");
    assert_eq!(
        h.pixel_in(7, 32, 32)[3],
        0,
        "newly allocated slots must start transparent"
    );
}

#[test]
fn a_hidden_layer_contributes_nothing() {
    let mut h = harness_or_skip!();
    h.fill(0, Color::WHITE);

    let mut hidden = layer(0, 1.0, BlendMode::Normal);
    hidden.visible = false;
    let px = h.composite_pixel(&[hidden], 32, 32);

    // Nothing drawn, so the transparency checkerboard shows through. Its two
    // greys are sRGB 0.88 and 0.78.
    assert!(
        px[0] > 190 && px[0] < 235,
        "expected the checkerboard, got {px:?}"
    );
}

#[test]
fn layer_opacity_blends_toward_what_is_beneath() {
    let mut h = harness_or_skip!();
    h.fill(0, Color::BLACK);
    h.fill(1, Color::WHITE);

    let stack = [
        layer(0, 1.0, BlendMode::Normal),
        layer(1, 0.5, BlendMode::Normal),
    ];
    let px = h.composite_pixel(&stack, 32, 32);

    // Half-opacity white over black is 0.5 in *linear* light, which displays
    // as sRGB ~188 — not 128. Asserting 128 here would be the classic
    // blend-in-gamma-space mistake.
    assert_near(px, [188, 188, 188], 4, "50% white over black");
}

#[test]
fn multiplying_by_white_is_the_identity() {
    // Chosen because it is exact: Multiply with a white top layer must leave
    // the layer beneath completely unchanged.
    let mut h = harness_or_skip!();
    let ink = Color::from_srgb_u8(120, 60, 30, 255);
    h.fill(0, ink);
    h.fill(1, Color::WHITE);

    let stack = [
        layer(0, 1.0, BlendMode::Normal),
        layer(1, 1.0, BlendMode::Multiply),
    ];
    assert_near(
        h.composite_pixel(&stack, 32, 32),
        [120, 60, 30],
        3,
        "multiply by white",
    );
}

#[test]
fn multiplying_by_black_yields_black() {
    let mut h = harness_or_skip!();
    h.fill(0, Color::from_srgb_u8(200, 200, 200, 255));
    h.fill(1, Color::BLACK);

    let stack = [
        layer(0, 1.0, BlendMode::Normal),
        layer(1, 1.0, BlendMode::Multiply),
    ];
    assert_near(
        h.composite_pixel(&stack, 32, 32),
        [0, 0, 0],
        3,
        "multiply by black",
    );
}

#[test]
fn screening_with_black_is_the_identity() {
    let mut h = harness_or_skip!();
    let ink = Color::from_srgb_u8(90, 140, 200, 255);
    h.fill(0, ink);
    h.fill(1, Color::BLACK);

    let stack = [
        layer(0, 1.0, BlendMode::Normal),
        layer(1, 1.0, BlendMode::Screen),
    ];
    assert_near(
        h.composite_pixel(&stack, 32, 32),
        [90, 140, 200],
        3,
        "screen with black",
    );
}

#[test]
fn screening_with_white_yields_white() {
    let mut h = harness_or_skip!();
    h.fill(0, Color::from_srgb_u8(60, 60, 60, 255));
    h.fill(1, Color::WHITE);

    let stack = [
        layer(0, 1.0, BlendMode::Normal),
        layer(1, 1.0, BlendMode::Screen),
    ];
    assert_near(
        h.composite_pixel(&stack, 32, 32),
        [255, 255, 255],
        2,
        "screen with white",
    );
}

#[test]
fn stack_order_decides_what_covers_what() {
    let mut h = harness_or_skip!();
    let red = Color::from_srgb_u8(220, 30, 30, 255);
    let blue = Color::from_srgb_u8(30, 30, 220, 255);
    h.fill(0, red);
    h.fill(1, blue);

    let blue_on_top = [
        layer(0, 1.0, BlendMode::Normal),
        layer(1, 1.0, BlendMode::Normal),
    ];
    assert_near(
        h.composite_pixel(&blue_on_top, 32, 32),
        [30, 30, 220],
        3,
        "blue over red",
    );

    // Same slots, reversed order — no pixels move, only the stack.
    let red_on_top = [
        layer(1, 1.0, BlendMode::Normal),
        layer(0, 1.0, BlendMode::Normal),
    ];
    assert_near(
        h.composite_pixel(&red_on_top, 32, 32),
        [220, 30, 30],
        3,
        "red over blue",
    );
}
