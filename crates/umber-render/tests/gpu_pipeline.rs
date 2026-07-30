//! End-to-end GPU tests: dab stamping, blending and stroke commit.
//!
//! These run headless (no surface) so they work in CI on any machine with a
//! working adapter, including software rasterisers like lavapipe. If no
//! adapter is available at all the tests skip rather than fail, so a machine
//! without a GPU doesn't produce noise.

use glam::UVec2;
use umber_core::{BrushMode, Color, Dab, PixelRect};
use umber_render::{CanvasRenderer, Gpu};

const DOC: u32 = 64;

struct Harness {
    gpu: Gpu,
    canvas: CanvasRenderer,
}

impl Harness {
    fn new() -> Option<Self> {
        let instance = Gpu::create_instance();
        let gpu = pollster::block_on(Gpu::new(instance, None)).ok()?;
        let canvas = CanvasRenderer::new(
            &gpu.device,
            UVec2::new(DOC, DOC),
            wgpu::TextureFormat::Rgba8UnormSrgb,
        );

        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        canvas.clear_layer(&mut enc);
        canvas.clear_stroke(&mut enc);
        gpu.queue.submit(Some(enc.finish()));

        Some(Self { gpu, canvas })
    }

    fn stamp(&mut self, dabs: &[Dab]) {
        let mut enc = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        self.canvas.begin_frame();
        self.canvas.draw_dabs(&self.gpu.queue, &mut enc, dabs);
        self.gpu.queue.submit(Some(enc.finish()));
    }

    fn commit(&mut self, color: Color, opacity: f32, mode: BrushMode) {
        let rect = PixelRect {
            x: 0,
            y: 0,
            width: DOC,
            height: DOC,
        };
        let mut enc = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        self.canvas
            .commit_stroke(&self.gpu.queue, &mut enc, rect, color, opacity, mode);
        self.gpu.queue.submit(Some(enc.finish()));
    }

    /// RGBA of a single layer pixel.
    fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let bytes = self.canvas.read_layer_rect(
            &self.gpu.device,
            &self.gpu.queue,
            PixelRect {
                x,
                y,
                width: 1,
                height: 1,
            },
        );
        [bytes[0], bytes[1], bytes[2], bytes[3]]
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

    // Commit again without stamping anything new.
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
    // sRGB 20, which is linear 0.0056, or 1.4/255 — it quantises to 1 and reads
    // back as sRGB 13. That is a visible jump in colour the moment the pointer
    // is released, and it is worst for exactly the dark colours people draw
    // with. An sRGB-encoded layer distributes precision perceptually instead.
    let mut h = harness_or_skip!();

    let ink = Color::from_srgb_u8(20, 20, 24, 255);
    h.stamp(&[dab(32.0, 32.0, 12.0, 1.0)]);
    h.commit(ink, 1.0, BrushMode::Paint);

    // The layer is sRGB-encoded, so the bytes read back are directly
    // comparable to the sRGB values that went in.
    let px = h.pixel(32, 32);
    assert_eq!(px[3], 255, "should be fully opaque");
    assert!(
        px[0].abs_diff(20) <= 2 && px[2].abs_diff(24) <= 2,
        "committed colour drifted from sRGB (20, 20, 24): got {px:?}"
    );
}

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
    let saved = h.canvas.read_layer_rect(&h.gpu.device, &h.gpu.queue, rect);
    assert_eq!(saved.len(), (rect.width * rect.height * 4) as usize);

    // Wipe, then restore — this is exactly the undo path.
    let mut enc = h
        .gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    h.canvas.clear_layer(&mut enc);
    h.gpu.queue.submit(Some(enc.finish()));
    assert_eq!(h.pixel(32, 32)[3], 0);

    h.canvas.write_layer_rect(&h.gpu.queue, rect, &saved);
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
    let bytes = h.canvas.read_layer_rect(&h.gpu.device, &h.gpu.queue, rect);
    assert_eq!(bytes.len(), 3 * 5 * 4);
    assert!(
        bytes.chunks(4).all(|p| p[3] == 255),
        "every pixel inside the dab should be opaque: {bytes:?}"
    );
}
