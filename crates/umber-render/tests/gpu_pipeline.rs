//! End-to-end GPU tests: dab stamping, blending, stroke commit and the layer
//! stack composite.
//!
//! These run headless (no surface) so they work in CI on any machine with a
//! working adapter, including software rasterisers like lavapipe. If no
//! adapter is available at all the tests skip rather than fail, so a machine
//! without a GPU doesn't produce noise.

use glam::{UVec2, Vec2};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use umber_core::{
    Anchor, Background, BlendMode, Brush, BrushMode, Camera, Color, Dab, DabInput, DabTarget,
    Effect, FlipAxis, InputPoint, Modulation, OutlinePosition, PixelRect, Rect, ResponseCurve,
    Selection, StrokeBuilder, TileMask, TipMask, Transform,
};
use umber_render::{
    BakedStack, CanvasRenderer, Choice, CompositeParams, DabStyle, DocumentCapture, EffectFrame,
    FloatParams, FloatSource, Gpu, LayerDraw, LayerEffects, ProbeParams, StrokeStyle, Thumbnail,
};

const DOC: u32 = 64;

/// Must match the real surface: non-sRGB, because `composite.wgsl` does the
/// gamma encode itself. Using an sRGB target here would encode twice and every
/// expected colour below would be wrong.
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// One device for the whole binary.
///
/// Creating a device per test meant a dozen-odd concurrent Vulkan devices on
/// one adapter, each blocking on `poll` waiting for its own submission. They
/// starve each other and the run never finishes — a hang, not a failure, which
/// is the worst way for CI to break.
fn shared_gpu() -> Option<&'static Gpu> {
    static GPU: OnceLock<Option<Gpu>> = OnceLock::new();
    GPU.get_or_init(|| {
        let instance = Gpu::create_instance();
        pollster::block_on(Gpu::with_adapter(instance, None, adapter_choice())).ok()
    })
    .as_ref()
}

/// `UMBER_TEST_SOFTWARE=1` runs the whole suite on the software rasteriser.
///
/// **This is how a failure on CI is reproduced before it is pushed.** GitHub's
/// runners have no graphics card, so every one of these tests runs there on
/// WARP or lavapipe, while the machine they were written on has a real one.
/// The two agree about what the shaders *do* and disagree in the last bit of
/// floating point, so a test asserting an exact byte can pass here and fail
/// there — which it did, on a tag that had already been pushed. Running
///
/// ```sh
/// UMBER_TEST_SOFTWARE=1 cargo test -p umber-render --test gpu_pipeline
/// ```
///
/// is the check that says whether an assertion is about this code or about the
/// hardware it happened to be written on. It is an environment variable rather
/// than a second test binary because the whole suite has to run under it, and
/// deliberately not the default: the hardware path is the one people paint on.
fn adapter_choice() -> Choice {
    match std::env::var_os("UMBER_TEST_SOFTWARE") {
        Some(v) if v != "0" => Choice::Fallback,
        _ => Choice::Best,
    }
}

struct Harness {
    /// Serialises GPU access. Held for the lifetime of the harness.
    _guard: MutexGuard<'static, ()>,
    gpu: &'static Gpu,
    canvas: CanvasRenderer,
    /// Carried so `commit` and `composite` agree with whatever `stamp_colored`
    /// last did. Preview and commit disagreeing about where the colour comes
    /// from is exactly the bug this pairing exists to prevent.
    per_dab_color: bool,
}

impl Harness {
    fn new() -> Option<Self> {
        static SERIAL: Mutex<()> = Mutex::new(());
        // Recover from poisoning: one failing test should not cascade into
        // every later one reporting a mutex error instead of its own result.
        let guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

        let gpu = shared_gpu()?;
        let mut canvas = CanvasRenderer::new(
            &gpu.device,
            &gpu.queue,
            UVec2::new(DOC, DOC),
            TARGET_FORMAT,
            1,
        );

        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        canvas.clear_all_layers(&gpu.queue);
        canvas.clear_stroke(&gpu.device, &mut enc);
        gpu.queue.submit(Some(enc.finish()));

        Some(Self {
            _guard: guard,
            gpu,
            canvas,
            per_dab_color: false,
        })
    }

    fn encoder(&self) -> wgpu::CommandEncoder {
        self.gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None })
    }

    fn stamp(&mut self, dabs: &[Dab]) {
        self.stamp_styled(dabs, DabStyle::default());
    }

    /// Stamp with the per-dab colour path on, as a smudging brush does.
    fn stamp_colored(&mut self, dabs: &[Dab], colored: bool) {
        self.stamp_styled(
            dabs,
            DabStyle {
                per_dab_color: colored,
                build_up: false,
            },
        );
    }

    /// Stamp with the build-up blend, as a texture stamp does.
    fn stamp_building(&mut self, dabs: &[Dab]) {
        self.stamp_styled(
            dabs,
            DabStyle {
                per_dab_color: false,
                build_up: true,
            },
        );
    }

    fn stamp_styled(&mut self, dabs: &[Dab], style: DabStyle) {
        self.per_dab_color = style.per_dab_color;
        let mut enc = self.encoder();
        self.canvas.begin_frame();
        self.canvas
            .draw_dabs(&self.gpu.device, &self.gpu.queue, &mut enc, dabs, style);
        self.gpu.queue.submit(Some(enc.finish()));
    }

    fn commit_to(&mut self, slot: u32, color: Color, opacity: f32, mode: BrushMode) {
        self.commit_blended_to(slot, color, opacity, mode, BlendMode::Normal);
    }

    /// The same commit with the brush carrying a blend mode.
    ///
    /// Separate from [`Harness::commit_to`] rather than an argument on it,
    /// because Normal is the path every other test in this file exercises and
    /// it must stay the one that is exercised by default.
    fn commit_blended_to(
        &mut self,
        slot: u32,
        color: Color,
        opacity: f32,
        mode: BrushMode,
        blend: BlendMode,
    ) {
        // The canvas's own size rather than `DOC`, so a test that has resized
        // still commits the whole scratch rather than its top-left corner.
        let size = self.canvas.doc_size();
        let rect = PixelRect {
            x: 0,
            y: 0,
            width: size.x,
            height: size.y,
        };
        let mut enc = self.encoder();
        self.canvas.commit_stroke(
            &self.gpu.device,
            &self.gpu.queue,
            &mut enc,
            slot,
            rect,
            &[rect],
            StrokeStyle {
                color,
                opacity,
                mode,
                blend,
                per_dab_color: self.per_dab_color,
                on_mask: false,
            },
        );
        self.gpu.queue.submit(Some(enc.finish()));
    }

    fn commit(&mut self, color: Color, opacity: f32, mode: BrushMode) {
        self.commit_to(0, color, opacity, mode);
    }

    /// The `Arc` is made here rather than at the call sites: the renderer
    /// compares tips by identity, and a fresh `Arc` per call is exactly the
    /// "this is a different brush" case each of these tests means.
    ///
    /// A coloured stamp's colour is honoured, which is what the editor decides
    /// for an ordinary paint stroke. [`Harness::set_tip_without_colour`] is the
    /// other answer — what an eraser and a stroke on a mask are given.
    fn set_tip(&mut self, tip: Option<TipMask>) {
        self.canvas
            .set_tip(&self.gpu.device, &self.gpu.queue, tip.map(Arc::new), true);
    }

    /// Bind a tip with its colour **refused**, as `begin_stroke` does for an
    /// eraser and for a stroke on a mask.
    fn set_tip_without_colour(&mut self, tip: Option<TipMask>) {
        self.canvas
            .set_tip(&self.gpu.device, &self.gpu.queue, tip.map(Arc::new), false);
    }

    /// The selection the dab pass is clipped to. The `Arc` is made here for
    /// the reason [`Harness::set_tip`]'s is: the renderer compares by identity,
    /// and a fresh `Arc` per call is the "this is a different selection" case
    /// each of these tests means.
    fn set_selection(&mut self, selection: Option<Selection>) {
        self.canvas
            .set_selection(&self.gpu.device, &self.gpu.queue, selection.map(Arc::new));
    }

    /// The paper, as `(tile, strength, tile size in document pixels)`.
    fn set_grain(&mut self, grain: Option<(TipMask, f32, f32)>) {
        self.canvas.set_grain(
            &self.gpu.device,
            &self.gpu.queue,
            grain.map(|(tile, strength, scale)| (Arc::new(tile), strength, scale)),
        );
    }

    fn set_background(&mut self, background: Background) {
        self.canvas.set_background(background);
    }

    /// This harness's canvas, through [`thumbnail_of`] — which is where the
    /// loop and its reasoning live, because one test drives a renderer of its
    /// own.
    fn thumbnail(&mut self, slot: u32) -> Thumbnail {
        thumbnail_of(self.gpu, &mut self.canvas, slot)
    }

    /// Put an exact block of bytes into a layer.
    ///
    /// Straight into the texture rather than through a dab, because the
    /// transform tests need a crisp rectangle whose every pixel is known: a
    /// stamped dab has an antialiased edge, and half the point of those tests is
    /// asserting where the edge of a moved region ended up.
    fn write_block(&mut self, slot: u32, rect: PixelRect, rgba: [u8; 4]) {
        let bytes: Vec<u8> = rgba
            .iter()
            .copied()
            .cycle()
            .take((rect.area() * 4) as usize)
            .collect();
        self.canvas
            .write_layer_rect(&self.gpu.device, &self.gpu.queue, slot, rect, &bytes);
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
        // No stroke in flight for most of these tests; zero opacity keeps the
        // scratch surface out of the result whatever it contains.
        self.composite_pixel_with(
            layers,
            StrokeStyle {
                opacity: 0.0,
                ..Default::default()
            },
            x,
            y,
        )
    }

    /// The same pass with a stroke in flight, which is how the *preview* half
    /// of anything the commit also does gets read.
    fn composite_pixel_with(
        &self,
        layers: &[LayerDraw],
        stroke: StrokeStyle,
        x: u32,
        y: u32,
    ) -> [u8; 4] {
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
                pivot: Vec2::splat(DOC as f32 * 0.5),
                layers,
                backdrop: [0.0, 0.0, 0.0],
                export: false,
                active_index: 0,
                stroke,
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
    Dab::round(Vec2::new(x, y), radius, 0.95, coverage)
}

fn coloured_dab(x: f32, y: f32, radius: f32, coverage: f32, color: [f32; 3]) -> Dab {
    Dab {
        color,
        ..Dab::round(Vec2::new(x, y), radius, 0.95, coverage)
    }
}

/// An elliptical dab: `aspect` long-over-short, `angle` in radians.
fn shaped_dab(x: f32, y: f32, radius: f32, aspect: f32, angle: f32) -> Dab {
    Dab {
        aspect,
        angle,
        ..Dab::round(Vec2::new(x, y), radius, 0.95, 1.0)
    }
}

fn layer(slot: u32, opacity: f32, blend: BlendMode) -> LayerDraw {
    LayerDraw {
        slot,
        opacity,
        blend: blend.index(),
        visible: true,
        mask: None,
        clipped: false,
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
fn a_pressure_step_finer_than_the_layer_makes_no_mark() {
    // How wide the wet-layer path actually is, end to end — and therefore what
    // widening the scratch would and would not buy.
    //
    // The scratch is `R8Unorm` and the layer is `Rgba8UnormSrgb`, whose *alpha*
    // channel is linear (sRGB formats encode RGB only). Both are 256 levels, so
    // the scratch is exactly as wide as its destination and adds no loss of its
    // own. That is the whole answer to "can a 16-bit scratch carry the pen's
    // 1024 pressure levels": no, because commit re-quantises to the same 256
    // whatever the scratch held. See the pressure note in CLAUDE.md.
    //
    // Base 0.4 rather than 0.5 deliberately: 0.4 * 255 is exactly 102, so
    // neither expectation sits on a rounding tie a driver may break either way.
    let mut h = harness_or_skip!();

    let base = 0.4f32;
    h.stamp(&[
        // One step of the pen's 1024 levels.
        dab(12.0, 12.0, 6.0, base),
        dab(12.0, 40.0, 6.0, base + 1.0 / 1023.0),
        // One step of the layer's 256.
        dab(40.0, 12.0, 6.0, base + 1.0 / 255.0),
    ]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);

    let plain = h.pixel(12, 12)[3];
    let pressure_step = h.pixel(12, 40)[3];
    let storage_step = h.pixel(40, 12)[3];

    assert_eq!(
        pressure_step, plain,
        "a 1/1024 pressure step must be invisible — the layer holds 256 alpha \
         levels, so a 16-bit scratch would not rescue it"
    );
    assert!(
        storage_step > plain,
        "a 1/255 step must still resolve: expected more than {plain}, got \
         {storage_step} — the path has become lossier than eight bits"
    );
    assert!(
        storage_step - plain <= 2,
        "a 1/255 step is one level, not {}",
        storage_step - plain
    );
}

// ---------------------------------------------------------------------------
// Build-up
// ---------------------------------------------------------------------------

/// What `n` dabs of coverage `c` composite to: `1 - (1 - c)^n`.
///
/// The model the build-up blend is supposed to implement, written out so the
/// tests below assert against the maths rather than against a number somebody
/// once read off a screen.
fn composited(coverage: f32, dabs: i32) -> f32 {
    1.0 - (1.0 - coverage).powi(dabs)
}

#[test]
fn building_dabs_reach_the_coverage_compositing_predicts() {
    // The whole point of the build-up mode. Eight quarter-coverage dabs on one
    // spot composite to 1 - 0.75^8 = 0.900, where a `max` cannot pass 0.25
    // however many are stamped.
    //
    // This is the mechanism a sparse texture stamp needs: the CC0 GIMP pack's
    // brightest texel is 0.49, so under a `max` a stroke of it can never be
    // more than half as strong as the author's however long it is. See
    // `docs/brush-sources.md`.
    let mut h = harness_or_skip!();

    let d = dab(32.0, 32.0, 12.0, 0.25);
    h.stamp_building(&[d; 8]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);

    let expected = (composited(0.25, 8) * 255.0) as u8;
    let alpha = h.pixel(32, 32)[3];
    assert!(
        alpha.abs_diff(expected) <= 6,
        "expected ~{expected} (1 - 0.75^8), got {alpha}"
    );
}

#[test]
fn build_up_leaves_the_max_path_alone() {
    // The other half of the evidence, and the half that matters more: the same
    // dabs through the ordinary pipeline must still saturate at one dab's
    // worth. Every brush in the library is on that path, and a build-up mode
    // that quietly changed it would be a regression across the whole set
    // wearing the name of a new feature.
    let mut h = harness_or_skip!();

    let d = dab(32.0, 32.0, 12.0, 0.25);
    h.stamp(&[d; 8]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);

    let expected = (0.25 * 255.0) as u8;
    let alpha = h.pixel(32, 32)[3];
    assert!(
        alpha.abs_diff(expected) <= 6,
        "expected ~{expected} (one dab's coverage), got {alpha} — the max blend has moved"
    );
}

#[test]
fn a_building_stroke_still_applies_its_opacity_exactly_once() {
    // Build-up must not become a second place stroke opacity is folded in.
    // Coverage builds to 0.900 and commit scales it by 0.5, once, so the
    // committed alpha is 0.450 — not 0.900 and not 0.125.
    let mut h = harness_or_skip!();

    let d = dab(32.0, 32.0, 12.0, 0.25);
    h.stamp_building(&[d; 8]);
    h.commit(Color::WHITE, 0.5, BrushMode::Paint);

    let expected = (composited(0.25, 8) * 0.5 * 255.0) as u8;
    let alpha = h.pixel(32, 32)[3];
    assert!(
        alpha.abs_diff(expected) <= 6,
        "expected ~{expected} (built coverage, halved once), got {alpha}"
    );
}

#[test]
fn a_building_stroke_saturates_rather_than_overflowing() {
    // `a + cov(1 - a)` can never pass 1.0, in exact arithmetic or in the
    // target's unorm. Worth pinning: an accumulation written as a plain `Add`
    // would look right for a few dabs and then clip, squaring the soft edge off
    // into a hard-edged blob.
    let mut h = harness_or_skip!();

    let d = dab(32.0, 32.0, 12.0, 0.5);
    h.stamp_building(&[d; 40]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);
    // 254, not 255. The scratch is `R8Unorm`, so once coverage reaches 254/255
    // the next dab contributes `0.5/255` and rounds to nothing: a partial dab
    // asymptotes one level short of solid. A dab of full coverage does reach
    // 255 — `a + 1(1 - a)` is exactly 1 — so this is the floor of the
    // quantisation and not a leak in the formula. An `R16Float` scratch would
    // close it, at twice the bandwidth of the hottest texture in the frame, to
    // remove a difference of 0.4%. See the pressure note in CLAUDE.md for the
    // measurement that decided against it.
    assert!(
        h.pixel(32, 32)[3] >= 254,
        "forty half-coverage dabs should be solid, got {}",
        h.pixel(32, 32)[3]
    );
    assert_eq!(
        h.pixel(CORNER, CORNER)[3],
        0,
        "the corner outside a round dab must stay empty"
    );
}

#[test]
fn a_building_tip_reaches_solid_where_a_max_one_cannot() {
    // The measurement from `docs/brush-sources.md`, in miniature: a stamp whose
    // strongest texel is 0.49 stamped repeatedly. Under a `max` the stroke
    // stops at 0.49 — half the author's mark — and under build-up it reaches
    // solid, which is what GIMP draws.
    let mut h = harness_or_skip!();

    // 125/255 = 0.490, the exact peak of `Organic/Organic_000.gbr`.
    let tip = TipMask::new(2, 2, vec![125; 4]).expect("tip");
    h.set_tip(Some(tip));

    let d = dab(32.0, 32.0, 12.0, 1.0);
    h.stamp(&[d; 12]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);
    let capped = h.pixel(32, 32)[3];
    assert!(
        capped.abs_diff(125) <= 3,
        "a max stroke cannot exceed the tip's brightest texel; got {capped}"
    );

    h.stamp_building(&[d; 12]);
    h.commit_to(1, Color::WHITE, 1.0, BrushMode::Paint);
    let built = h.pixel_in(1, 32, 32)[3];
    assert!(
        built >= 250,
        "twelve stamps at 0.49 should composite to solid; got {built}"
    );
}

#[test]
fn a_building_smudge_keeps_its_colour_and_its_accumulation() {
    // The fourth pipeline, and the combination worth pinning because it is the
    // one that could have been refused. Coverage builds while colour still
    // blends premultiplied `over` — and under build-up the two attachments
    // agree exactly, because `over` accumulates the colour target's alpha by
    // the same formula the coverage target now uses. The un-premultiply in
    // `commit.wgsl` is therefore dividing by the coverage that is really there,
    // which is what makes the smear come out the later dabs' colour at the
    // built-up strength rather than washed towards the palette.
    let mut h = harness_or_skip!();

    let d = |c: [f32; 3]| coloured_dab(32.0, 32.0, 12.0, 0.25, c);
    h.stamp_styled(
        &[
            d([1.0, 0.0, 0.0]),
            d([1.0, 0.0, 0.0]),
            d([0.0, 0.0, 1.0]),
            d([0.0, 0.0, 1.0]),
        ],
        DabStyle {
            per_dab_color: true,
            build_up: true,
        },
    );
    h.commit(Color::BLACK, 1.0, BrushMode::Paint);

    let px = h.pixel(32, 32);
    let expected = (composited(0.25, 4) * 255.0) as u8;
    assert!(
        px[3].abs_diff(expected) <= 6,
        "expected ~{expected} of coverage, got {}",
        px[3]
    );
    assert!(
        px[2] > px[0],
        "the later blue dabs should dominate the smear: {px:?}"
    );
}

#[test]
fn a_smudging_stroke_commits_the_colour_its_dabs_carried() {
    // The per-dab colour path. `commit` is given black as the stroke colour;
    // if the colour scratch were ignored — the flag unset, the texture unbound,
    // the un-premultiply wrong — the mark would come out black rather than red.
    let mut h = harness_or_skip!();

    h.stamp_colored(
        &[coloured_dab(32.0, 32.0, 12.0, 1.0, [1.0, 0.0, 0.0])],
        true,
    );
    h.commit(Color::BLACK, 1.0, BrushMode::Paint);

    // sRGB 255 for linear 1.0; the layer stores sRGB-encoded bytes.
    assert_near(h.pixel(32, 32), [255, 0, 0], 4, "smudged dab");
}

#[test]
fn smudged_dabs_still_do_not_compound() {
    // The wet-layer guarantee has to survive the second attachment. Coverage
    // keeps its `max` blend while colour blends `over`, and it would be easy to
    // give both the same state — at which point a blender scrubbed back and
    // forth over one spot would darken with every pass.
    let mut h = harness_or_skip!();

    let d = coloured_dab(32.0, 32.0, 12.0, 0.5, [1.0, 0.0, 0.0]);
    h.stamp_colored(&[d, d, d, d], true);
    h.commit(Color::BLACK, 1.0, BrushMode::Paint);

    let alpha = h.pixel(32, 32)[3];
    assert!(
        (100..=155).contains(&alpha),
        "expected ~128 (single coverage), got {alpha} — smudged dabs are compounding"
    );
}

/// A canvas too large to speculate on gives the per-dab colour scratch back
/// when the stroke that used it ends, and the next smudge still paints.
///
/// 800 MB at 100 Mpx, held for the session after one smudging stroke. The
/// threshold is `GROWTH_DOUBLING_BUDGET_BYTES` — this codebase's own test for
/// "too large to guess on somebody's behalf" — which a real canvas reaches at
/// about 8192². `set_speculation_limit` is how a document small enough to check
/// by hand drives it, exactly as `set_readback_limit` drives the banded reader.
///
/// **Both directions, because either alone agrees with itself.** Under the
/// limit the texture must survive, or an ordinary document reallocates a
/// canvas-sized texture at the start of every blending stroke; over it the
/// texture must go, *and the mark it makes afterwards must be unchanged*, which
/// is the pixel this whole item is not allowed to move.
#[test]
fn a_large_canvas_gives_the_colour_scratch_back_when_a_stroke_ends() {
    let mut h = harness_or_skip!();
    let red = coloured_dab(32.0, 32.0, 12.0, 1.0, [1.0, 0.0, 0.0]);

    // Under the limit: the ordinary case, and it must not change.
    h.stamp_colored(&[red], true);
    assert!(
        h.canvas.holds_stroke_color(),
        "a smudging stroke recorded no colour"
    );
    h.commit(Color::BLACK, 1.0, BrushMode::Paint);
    // The centre and a texel out at the antialiased rim. The rim is where the
    // composite's *bilinear* tap on the colour plane can reach a texel no
    // fragment wrote, which is the reason `ensure_stroke_color` clears what it
    // allocates — this reading is the only thing that would notice if a
    // reallocated plane came back holding whatever the driver left. It is not
    // fully discriminating: an adapter that zeroes a fresh texture passes either
    // way, which is exactly what makes the clear worth having rather than the
    // test.
    let ordinary = h.pixel(32, 32);
    let rim = h.pixel(32, 20);
    assert_near(ordinary, [255, 0, 0], 4, "a smudged dab under the limit");
    assert!(
        h.canvas.holds_stroke_color(),
        "an ordinary canvas gave the colour scratch back, so every blending \
         stroke now reallocates one"
    );

    // Over it. Zero rather than a contrived figure: no canvas has a slice of
    // no bytes, so this is "always too large" and says so.
    h.canvas.set_speculation_limit(0);
    let mut enc = h.encoder();
    h.canvas.clear_stroke(&h.gpu.device, &mut enc);
    h.gpu.queue.submit(Some(enc.finish()));
    assert!(
        !h.canvas.holds_stroke_color(),
        "a canvas too large to speculate on held the colour scratch anyway"
    );

    // And the next stroke paints the same mark, out of a texture that had to be
    // allocated again. `clear_layer` rather than `Harness::fill`, which stamps a
    // dab and commits it at full opacity whatever colour it is handed — the
    // first draft of this used `fill` with a transparent colour and painted
    // opaque black over the mark it was about to compare against.
    let enc = h.encoder();
    h.canvas.clear_layer(&h.gpu.queue, 0);
    h.gpu.queue.submit(Some(enc.finish()));
    h.stamp_colored(&[red], true);
    assert!(h.canvas.holds_stroke_color());
    h.commit(Color::BLACK, 1.0, BrushMode::Paint);
    assert_eq!(
        h.pixel(32, 32),
        ordinary,
        "the mark moved when the colour scratch was reallocated"
    );
    assert_eq!(
        h.pixel(32, 20),
        rim,
        "the mark's edge moved when the colour scratch was reallocated"
    );
}

#[test]
fn later_dabs_win_the_colour_where_a_smudge_crosses_itself() {
    // Colour blends `over`, so a pixel ends up wearing the most recent dab that
    // covered it. That is what makes a smear trail along a stroke instead of
    // averaging everything the brush picked up over its whole length.
    let mut h = harness_or_skip!();

    h.stamp_colored(
        &[
            coloured_dab(32.0, 32.0, 12.0, 1.0, [1.0, 0.0, 0.0]),
            coloured_dab(32.0, 32.0, 12.0, 1.0, [0.0, 0.0, 1.0]),
        ],
        true,
    );
    h.commit(Color::BLACK, 1.0, BrushMode::Paint);

    assert_near(h.pixel(32, 32), [0, 0, 255], 4, "last dab's colour");
}

#[test]
fn an_ordinary_stroke_ignores_the_colour_scratch_entirely() {
    // The fast path must stay exactly as it was. A dab carrying a red colour
    // that is *not* stamped through the coloured pipeline has to commit as the
    // stroke colour — otherwise the flag is being ignored somewhere and every
    // ordinary stroke would start paying for a feature it does not use.
    let mut h = harness_or_skip!();

    h.stamp_colored(
        &[coloured_dab(32.0, 32.0, 12.0, 1.0, [1.0, 0.0, 0.0])],
        false,
    );
    h.commit(Color::new(0.0, 1.0, 0.0, 1.0), 1.0, BrushMode::Paint);

    assert_near(
        h.pixel(32, 32),
        [0, 255, 0],
        4,
        "stroke colour, not the dab's",
    );
}

#[test]
fn an_elliptical_dab_is_wide_along_its_angle_and_narrow_across_it() {
    // A chisel has to be a chisel. `size` describes the *long* axis whatever
    // the aspect, so raising the ratio narrows the dab rather than growing it —
    // which is what lets a flat brush cover the same ground as the round one it
    // was derived from.
    let mut h = harness_or_skip!();

    // 4:1, long axis along +x. Semi-axes are then 24 px by 6 px.
    h.stamp(&[shaped_dab(32.0, 32.0, 24.0, 4.0, 0.0)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);

    assert_eq!(h.pixel(32, 32)[3], 255, "centre should be solid");
    assert_eq!(h.pixel(50, 32)[3], 255, "18 px along should be inside");
    assert_eq!(h.pixel(32, 36)[3], 255, "4 px across is still inside the 6");
    assert_eq!(h.pixel(32, 42)[3], 0, "10 px across is past the short axis");
    assert_eq!(h.pixel(56, 32)[3], 0, "24 px along is past the long one");
}

#[test]
fn rotating_a_dab_rotates_the_ellipse() {
    // The same dab turned a quarter turn: what was wide is now tall. A rake
    // that ignored its angle would look identical whichever way it travelled.
    let mut h = harness_or_skip!();

    h.stamp(&[shaped_dab(
        32.0,
        32.0,
        24.0,
        4.0,
        std::f32::consts::FRAC_PI_2,
    )]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);

    assert_eq!(h.pixel(32, 50)[3], 255, "18 px down should now be inside");
    assert_eq!(
        h.pixel(42, 32)[3],
        0,
        "10 px sideways is now past the short axis"
    );
}

#[test]
fn a_jittered_angle_spreads_a_stroke_the_way_a_fixed_one_cannot() {
    // The whole path for the shape mapping that most of the pack asks for:
    // `Brush` says the dab may turn, `StrokeBuilder` rolls an angle per dab,
    // and the vertex shader builds each quad rotated. A stroke of long dabs
    // all lying the same way covers a band as thin as the *short* axis; one
    // that turns covers ground out to the long axis.
    //
    // Asserted as coverage where there would otherwise be none, rather than as
    // a pixel value, because the point is the footprint and not the blend.
    let mut h = harness_or_skip!();

    let comb = Brush {
        size: 24.0,
        spacing: 0.25,
        stabilization: 0.0,
        pressure_size: false,
        hardness: 1.0,
        dab_ratio: 6.0,
        ..Default::default()
    };
    let paint = |h: &mut Harness, brush: Brush| {
        let mut s = StrokeBuilder::new();
        s.begin(
            brush,
            [1.0, 1.0, 1.0],
            InputPoint::new(Vec2::new(14.0, 32.0), 1.0, 0.0),
        );
        s.extend(InputPoint::new(Vec2::new(50.0, 32.0), 1.0, 0.1));
        let dabs: Vec<Dab> = s.drain_pending().collect();
        h.stamp(&dabs);
        h.commit(Color::WHITE, 1.0, BrushMode::Paint);
        // 10 px above the line: outside the 2 px short semi-axis, well inside
        // the 12 px long one.
        (16..48).map(|x| h.pixel(x, 22)[3]).max().unwrap_or(0)
    };

    assert_eq!(
        paint(&mut h, comb),
        0,
        "dabs all lying along the stroke should not reach 10 px off it"
    );

    let enc = h.encoder();
    h.canvas.clear_all_layers(&h.gpu.queue);
    h.gpu.queue.submit(Some(enc.finish()));

    let grain = Brush {
        dab_angle_jitter: 360.0,
        ..comb
    };
    assert!(
        paint(&mut h, grain) > 0,
        "a dab free to turn should reach out towards its long axis"
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
// Per-brush blend modes
// ---------------------------------------------------------------------------

/// Every mode a brush can carry that is not the plain source-over every stroke
/// used to be. Normal is deliberately not here: it stays on the fixed-function
/// path and is what the rest of this file exercises.
const BRUSH_BLENDS: [BlendMode; 4] = [
    BlendMode::Multiply,
    BlendMode::Screen,
    BlendMode::Overlay,
    BlendMode::Add,
];

/// Every mode the engine has, which is what the layer stack can be set to.
///
/// Taken from `BlendMode::ALL` rather than written out, so a mode added to the
/// enum is one this file starts driving without anybody remembering to add it.
fn every_blend_mode() -> impl Iterator<Item = BlendMode> {
    BlendMode::ALL.into_iter()
}

/// The whole document as a rectangle.
fn whole(h: &Harness) -> PixelRect {
    let size = h.canvas.doc_size();
    PixelRect {
        x: 0,
        y: 0,
        width: size.x,
        height: size.y,
    }
}

/// Back to an empty canvas and an empty scratch, between one mode and the next.
fn reset(h: &mut Harness) {
    let mut enc = h.encoder();
    h.canvas.clear_all_layers(&h.gpu.queue);
    h.canvas.clear_stroke(&h.gpu.device, &mut enc);
    h.gpu.queue.submit(Some(enc.finish()));
}

/// **The test this whole feature hangs on.**
///
/// `composite.wgsl` draws the stroke as a live preview and `commit.wgsl`
/// replaces it at pointer-up. CLAUDE.md's rule is that the two must implement
/// identical blending maths, because any difference between them is a mark that
/// visibly jumps under the artist's hand at the moment they lift the pen. They
/// now share one `composite_over` out of `blend.wgsl`, which makes the *maths*
/// structurally the same; this is what says the wiring around it — the uniform,
/// the backdrop copy, which entry point is drawn — agrees as well.
///
/// Compared at the middle of a solid dab over an opaque layer, so coverage is
/// exactly 1 and the colour is not a nearly transparent pixel's. The tolerance
/// is what eight bits of sRGB storage can round by: the preview never leaves
/// float, the commit goes through the layer.
#[test]
fn a_blended_stroke_previews_exactly_as_it_commits() {
    let mut h = harness_or_skip!();

    // Something with colour underneath, or Multiply and Screen would be reading
    // bare canvas and every mode would agree by accident.
    let under = [90u8, 140, 200, 255];
    let ink = Color::from_srgb_u8(230, 120, 40, 255);

    for blend in BRUSH_BLENDS {
        reset(&mut h);
        let rect = whole(&h);
        h.write_block(0, rect, under);
        h.stamp(&[dab(32.0, 32.0, 16.0, 1.0)]);

        let stack = [layer(0, 1.0, BlendMode::Normal)];
        let style = StrokeStyle {
            color: ink,
            opacity: 1.0,
            mode: BrushMode::Paint,
            blend,
            per_dab_color: false,
            on_mask: false,
        };
        let previewed = h.composite_pixel_with(&stack, style, 32, 32);
        h.commit_blended_to(0, ink, 1.0, BrushMode::Paint, blend);
        let committed = h.composite_pixel(&stack, 32, 32);

        assert_near(
            committed,
            [previewed[0], previewed[1], previewed[2]],
            2,
            &format!("{blend:?} jumped at pointer-up"),
        );
    }
}

/// The same, for a *smudging* stroke — the one that reads its colour per dab.
///
/// The maths is shared, so this is not in doubt; the wiring is, and this is the
/// half of it the test above cannot reach. `per_dab_color` picks a different
/// dab pipeline writing a second attachment, and it picks a different *branch*
/// in both `composite.wgsl` and `commit.wgsl` for where the stroke's colour
/// comes from. A blended commit that read the flat palette colour where the
/// preview read the scratch would agree on every test above and disagree here,
/// and the artist would see a smear jump to the palette colour at pointer-up.
#[test]
fn a_blended_smudging_stroke_previews_exactly_as_it_commits() {
    let mut h = harness_or_skip!();

    let under = [70u8, 160, 110, 255];
    // Deliberately not the palette colour below, so a commit reading the wrong
    // one of the two is a visible difference rather than a coincidence.
    let picked = [0.85f32, 0.25, 0.55];
    let palette = Color::from_srgb_u8(20, 20, 240, 255);

    for blend in BRUSH_BLENDS {
        reset(&mut h);
        let rect = whole(&h);
        h.write_block(0, rect, under);
        h.stamp_colored(&[coloured_dab(32.0, 32.0, 16.0, 1.0, picked)], true);

        let stack = [layer(0, 1.0, BlendMode::Normal)];
        let style = StrokeStyle {
            color: palette,
            opacity: 1.0,
            mode: BrushMode::Paint,
            blend,
            per_dab_color: true,
            on_mask: false,
        };
        let previewed = h.composite_pixel_with(&stack, style, 32, 32);
        h.commit_blended_to(0, palette, 1.0, BrushMode::Paint, blend);
        let committed = h.composite_pixel(&stack, 32, 32);

        assert_near(
            committed,
            [previewed[0], previewed[1], previewed[2]],
            2,
            &format!("{blend:?} jumped at pointer-up on a smudging stroke"),
        );
    }
}

/// A stroke on a mask ignores the blend mode it is carrying, exactly as an
/// eraser does.
///
/// A mask slice holds coverage on one channel and `fs_blend` writes four, so a
/// blended commit onto a mask would put colour into it. `commit_stroke` refuses
/// the blended path for `on_mask` for that reason — the editor never sends one,
/// but this is the renderer's own guard rather than a promise about its caller,
/// and defending the eraser and not this was the asymmetry that would be
/// forgotten first.
#[test]
fn a_stroke_on_a_mask_ignores_the_blend_mode_it_is_carrying() {
    let mut h = harness_or_skip!();
    h.canvas.ensure_slots(&h.gpu.device, &h.gpu.queue, 4);

    // Grey on grey, deliberately, and this is the whole of what makes the test
    // able to fail. A mask is read on `.r`, so black paint on a white mask is
    // the one pair Multiply cannot tell from Normal — `0 × 1` and `0` are the
    // same number, and the assertion would hold with the guard taken out. Half
    // way up the channel it does not: `0.216 × 0.216` is nowhere near `0.216`.
    let ink = Color::from_srgb_u8(128, 128, 128, 255);
    let ground = [128u8, 128, 128, 255];

    let rect = whole(&h);
    let style = |blend, on_mask| StrokeStyle {
        color: ink,
        opacity: 1.0,
        mode: BrushMode::Paint,
        blend,
        per_dab_color: false,
        on_mask,
    };

    let commit = |h: &mut Harness, slot: u32, blend, on_mask| {
        fill_slot(h, slot, ground);
        h.stamp(&[dab(32.0, 32.0, 14.0, 1.0)]);
        let mut enc = h.encoder();
        h.canvas.commit_stroke(
            &h.gpu.device,
            &h.gpu.queue,
            &mut enc,
            slot,
            rect,
            &[rect],
            style(blend, on_mask),
        );
        h.gpu.queue.submit(Some(enc.finish()));
    };

    // The two masks differ only in the blend mode the style carries.
    commit(&mut h, 1, BlendMode::Normal, true);
    commit(&mut h, 3, BlendMode::Multiply, true);
    // And the same pair on an ordinary layer, which is what says these colours
    // genuinely distinguish the two modes. Without this the assertion above
    // would be satisfied by a Multiply that did nothing.
    commit(&mut h, 0, BlendMode::Normal, false);
    commit(&mut h, 2, BlendMode::Multiply, false);

    assert_ne!(
        h.pixel_in(0, 32, 32),
        h.pixel_in(2, 32, 32),
        "these colours cannot tell Multiply from Normal, so the mask assertion \
         below proves nothing — pick different ones"
    );
    assert_eq!(
        h.pixel_in(1, 32, 32),
        h.pixel_in(3, 32, 32),
        "a blend mode changed what a stroke on a mask did"
    );
}

/// The same, for a stroke that is only half opaque.
///
/// Stroke opacity is applied exactly once, at commit — the invariant the wet
/// layer exists for — and a blend mode must not become the place it is applied
/// twice. A partial alpha is also where the two halves of the W3C formula
/// (`(1 - ab)*Sc` and `as*ab*B`) both carry weight, so a preview that dropped
/// one of them would show here and nowhere else.
#[test]
fn a_blended_stroke_at_partial_opacity_previews_as_it_commits() {
    let mut h = harness_or_skip!();

    let under = [200u8, 60, 60, 255];
    let ink = Color::from_srgb_u8(40, 90, 220, 255);

    for blend in BRUSH_BLENDS {
        reset(&mut h);
        let rect = whole(&h);
        h.write_block(0, rect, under);
        h.stamp(&[dab(32.0, 32.0, 16.0, 1.0)]);

        let stack = [layer(0, 1.0, BlendMode::Normal)];
        let style = StrokeStyle {
            color: ink,
            opacity: 0.5,
            mode: BrushMode::Paint,
            blend,
            per_dab_color: false,
            on_mask: false,
        };
        let previewed = h.composite_pixel_with(&stack, style, 32, 32);
        h.commit_blended_to(0, ink, 0.5, BrushMode::Paint, blend);
        let committed = h.composite_pixel(&stack, 32, 32);

        assert_near(
            committed,
            [previewed[0], previewed[1], previewed[2]],
            2,
            &format!("{blend:?} at half opacity jumped at pointer-up"),
        );
    }
}

/// Blend identities rather than hand-computed values, for the reason CLAUDE.md
/// gives: they are exact and they survive rounding on either adapter.
///
/// Multiplying white onto a picture leaves it alone, and screening black onto
/// it does too. Both are the *brush* doing it, so they also say the brush's
/// mode is reaching the commit at all — a mode quietly ignored would pass a
/// test that only checked the mark had changed.
#[test]
fn a_blended_brush_keeps_the_identities_its_mode_promises() {
    let mut h = harness_or_skip!();
    let under = [70u8, 160, 110, 255];

    for (blend, ink, what) in [
        (BlendMode::Multiply, Color::WHITE, "multiplying by white"),
        (BlendMode::Screen, Color::BLACK, "screening with black"),
    ] {
        reset(&mut h);
        let rect = whole(&h);
        h.write_block(0, rect, under);
        h.stamp(&[dab(32.0, 32.0, 16.0, 1.0)]);
        h.commit_blended_to(0, ink, 1.0, BrushMode::Paint, blend);

        let px = h.pixel(32, 32);
        assert_near(
            px,
            [under[0], under[1], under[2]],
            1,
            &format!("{what} is the identity"),
        );
        assert_eq!(px[3], 255, "{what} left the alpha alone");
    }
}

/// A blend mode reads what is *underneath*, and on bare canvas there is
/// nothing: W3C's formula collapses to the source, so a multiplying brush on an
/// empty layer lays down its own colour rather than nothing at all.
///
/// The obvious wrong implementation — multiplying by a backdrop read as opaque
/// black — paints an invisible stroke, and that is the shape this catches.
#[test]
fn a_multiplying_brush_on_bare_canvas_lays_down_its_own_colour() {
    let mut h = harness_or_skip!();

    h.stamp(&[dab(32.0, 32.0, 16.0, 1.0)]);
    h.commit_blended_to(
        0,
        Color::from_srgb_u8(220, 90, 30, 255),
        1.0,
        BrushMode::Paint,
        BlendMode::Multiply,
    );

    let px = h.pixel(32, 32);
    assert_eq!(px[3], 255, "the stroke should be there at all");
    assert_near(
        px,
        [220, 90, 30],
        2,
        "a multiply over nothing is the source",
    );
}

/// An eraser carrying a blend mode erases exactly as one that is not.
///
/// A blend mode is a rule for combining a colour with what is under it, and an
/// eraser deposits none — it is a different *blend state*, `src_factor: Zero`,
/// which is the invariant `erasing_removes_coverage` guards. So the mode is
/// ignored rather than approximated, the editor never sends one down here, and
/// this is what says the renderer would not honour one if it did.
#[test]
fn an_erasing_brush_ignores_the_blend_mode_it_is_carrying() {
    let mut h = harness_or_skip!();

    h.stamp(&[dab(32.0, 32.0, 14.0, 1.0)]);
    h.commit_to(0, Color::WHITE, 1.0, BrushMode::Paint);
    h.stamp(&[dab(32.0, 32.0, 14.0, 1.0)]);
    h.commit_to(1, Color::WHITE, 1.0, BrushMode::Paint);
    assert_eq!(h.pixel_in(0, 32, 32), h.pixel_in(1, 32, 32));

    h.stamp(&[dab(32.0, 32.0, 10.0, 0.5)]);
    h.commit_to(0, Color::WHITE, 1.0, BrushMode::Erase);
    h.stamp(&[dab(32.0, 32.0, 10.0, 0.5)]);
    h.commit_blended_to(1, Color::WHITE, 1.0, BrushMode::Erase, BlendMode::Multiply);

    assert_eq!(
        h.pixel_in(0, 32, 32),
        h.pixel_in(1, 32, 32),
        "a blend mode changed what an eraser did"
    );
}

/// The blended commit is scissored to the same pieces the undo patch was
/// captured from, exactly as the plain one is.
///
/// It gets there differently — a pass per piece with its own backdrop copy,
/// rather than one pass with a scissor per piece — so the guarantee has to be
/// re-stated for it. Two pieces with a gap between them also drive the
/// per-piece uniform, which is the one thing in this path a single-piece commit
/// would never exercise: if the dynamic offset were wrong, the second piece
/// would sample the first's backdrop and the assertions below would disagree.
#[test]
fn a_blended_commit_writes_only_the_pieces_it_was_given() {
    let mut h = harness_or_skip!();

    let rect = whole(&h);
    h.write_block(0, rect, [200, 200, 200, 255]);
    // A dab wide enough to cover both pieces and the gap between them, so what
    // decides where paint lands is the piece list and nothing else.
    h.stamp(&[dab(32.0, 32.0, 40.0, 1.0)]);

    let pieces = [
        PixelRect {
            x: 8,
            y: 24,
            width: 12,
            height: 16,
        },
        PixelRect {
            x: 44,
            y: 24,
            width: 12,
            height: 16,
        },
    ];
    let mut enc = h.encoder();
    h.canvas.commit_stroke(
        &h.gpu.device,
        &h.gpu.queue,
        &mut enc,
        0,
        rect,
        &pieces,
        StrokeStyle {
            color: Color::BLACK,
            opacity: 1.0,
            mode: BrushMode::Paint,
            blend: BlendMode::Multiply,
            per_dab_color: false,
            on_mask: false,
        },
    );
    h.gpu.queue.submit(Some(enc.finish()));

    // Multiplying black is black, in both pieces.
    for x in [12u32, 48] {
        assert_near(
            h.pixel(x, 32),
            [0, 0, 0],
            2,
            "a piece the commit was given was not painted",
        );
    }
    // The gap between them, and a row above both, are untouched.
    for (x, y) in [(32u32, 32u32), (12, 8), (48, 8)] {
        assert_eq!(
            h.pixel(x, y),
            [200, 200, 200, 255],
            "a pixel outside every piece was written at ({x}, {y})"
        );
    }
}

// ---------------------------------------------------------------------------
// Bitmap tips
// ---------------------------------------------------------------------------

/// A pixel inside a radius-12 dab's bounding square but outside its circle.
///
/// The dab at (32, 32) spans 20..44 on both axes, so pixel 21's centre sits at
/// local (-0.875, -0.875): a distance of 1.24 from the centre, comfortably past
/// the falloff's outer edge at 1.0, and comfortably inside the square the tip
/// covers.
const CORNER: u32 = 21;

#[test]
fn a_tipped_stamp_still_saturates_under_overlap() {
    // The tip version of `overlapping_dabs_do_not_compound`, and the reason
    // that test is worth copying rather than extending: a tip modulates
    // coverage, it does not composite. If the tip were ever blended in rather
    // than selected between, the `max` blend would stop saturating and tipped
    // strokes would go blotchy wherever they cross themselves.
    let mut h = harness_or_skip!();

    let tip = TipMask::new(2, 2, vec![255; 4]).expect("tip");
    h.set_tip(Some(tip));

    h.stamp(&[dab(32.0, 32.0, 12.0, 0.5), dab(32.0, 32.0, 12.0, 0.5)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);

    let alpha = h.pixel(32, 32)[3];
    assert!(
        (100..=155).contains(&alpha),
        "expected ~128 (single coverage), got {alpha} — tipped dabs are compounding"
    );
}

#[test]
fn a_tip_decides_where_paint_lands() {
    // A two-texel tip, opaque on the left and empty on the right, stretched
    // over the dab's bounding square. If the tip were ignored, or sampled with
    // the axes swapped, both sides would come out the same.
    let mut h = harness_or_skip!();

    let tip = TipMask::new(2, 1, vec![255, 0]).expect("tip");
    h.set_tip(Some(tip));

    // Radius 12 at x = 32 spans 20..44, so x = 22 and x = 42 sit well inside
    // the left and right texels rather than in the interpolated middle.
    h.stamp(&[dab(32.0, 32.0, 12.0, 1.0)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);

    assert_eq!(
        h.pixel(22, 32)[3],
        255,
        "the tip's opaque half should paint"
    );
    assert_eq!(h.pixel(42, 32)[3], 0, "the tip's empty half should not");
}

#[test]
fn a_stamp_gets_the_same_shape_dynamics_a_round_dab_does() {
    // Scatter, size jitter and angle jitter are per-dab fields on an instance
    // the vertex shader shapes *before* it samples the tip, so a stamp brush
    // gets all three for free — but "for free" is exactly the kind of claim
    // that stops being true. A spatter brush that laid every stamp on the line
    // at one size would be a texture with a ruler through it.
    //
    // Asserted first as dabs landing off the line and off the nominal size,
    // then — the part that matters — as the scratch being *empty* once the
    // stroke has been committed over the rect the builder reported. That is the
    // end-to-end form of "the damaged rect is wide enough": anything the rect
    // missed is still sitting in the scratch, and the next commit would bake it
    // in wearing the next stroke's colour. A tip reaches into its quad's
    // corners, so a rotated, scattered stamp is the hardest case there is.
    let mut h = harness_or_skip!();
    h.set_tip(Some(TipMask::new(2, 2, vec![255; 4]).expect("tip")));

    let spatter = Brush {
        size: 8.0,
        spacing: 0.5,
        stabilization: 0.0,
        pressure_size: false,
        scatter: 2.5,
        radius_jitter: 0.4,
        dab_angle_jitter: 360.0,
        ..Default::default()
    };

    let mut s = StrokeBuilder::new();
    s.begin(
        spatter,
        [1.0, 1.0, 1.0],
        InputPoint::new(Vec2::new(20.0, 32.0), 1.0, 0.0),
    );
    s.extend(InputPoint::new(Vec2::new(44.0, 32.0), 1.0, 0.1));
    let dabs: Vec<Dab> = s.drain_pending().collect();
    let bounds = s.bounds();

    // 4 px radius on the line, so nothing unscattered can reach 8 px off it.
    let off_line = dabs
        .iter()
        .any(|d| (d.pos[1] - 32.0).abs() > 8.0 || (d.radius - 4.0).abs() > 0.5);
    assert!(off_line, "the stamp is not being scattered or jittered");

    h.stamp(&dabs);
    let rect = bounds.to_pixels_clamped(UVec2::splat(DOC)).expect("rect");
    let mut enc = h.encoder();
    h.canvas.commit_stroke(
        &h.gpu.device,
        &h.gpu.queue,
        &mut enc,
        0,
        rect,
        &[rect],
        StrokeStyle {
            color: Color::WHITE,
            ..Default::default()
        },
    );
    h.gpu.queue.submit(Some(enc.finish()));

    // Nothing may be left in the scratch: whatever it still held would redraw
    // as a preview and be baked into the next stroke.
    h.commit_to(1, Color::WHITE, 1.0, BrushMode::Paint);
    let leftover = (0..DOC)
        .flat_map(|y| (0..DOC).map(move |x| (x, y)))
        .any(|(x, y)| h.pixel_in(1, x, y)[3] != 0);
    assert!(!leftover, "the spatter left coverage outside its own rect");
}

#[test]
fn a_tip_paints_the_corners_a_round_brush_leaves_alone() {
    // The clearest evidence the procedural falloff has been replaced rather
    // than multiplied into: a full tip covers its whole bounding square, where
    // a round dab of the same radius cannot reach the corners at all.
    let mut h = harness_or_skip!();

    h.stamp(&[dab(32.0, 32.0, 12.0, 1.0)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);
    assert_eq!(
        h.pixel_in(0, CORNER, CORNER)[3],
        0,
        "a round dab must not reach its bounding square's corner"
    );

    let tip = TipMask::new(2, 2, vec![255; 4]).expect("tip");
    h.set_tip(Some(tip));
    h.stamp(&[dab(32.0, 32.0, 12.0, 1.0)]);
    h.commit_to(1, Color::WHITE, 1.0, BrushMode::Paint);
    assert_eq!(
        h.pixel_in(1, CORNER, CORNER)[3],
        255,
        "a full tip must cover the whole square"
    );
}

#[test]
fn a_non_square_tip_keeps_its_proportions() {
    // A 4:1 landscape mask, solid. It has to land four times as wide as it is
    // tall: stretched over the dab's square it would be a block, and padded
    // into a square it would be this shape at the cost of a margin of empty
    // fragments and four times the texture.
    //
    // `Brush::size` describes the long axis, so the mask's long side spans the
    // full 2 * radius and its short side a quarter of that.
    let mut h = harness_or_skip!();

    h.set_tip(Some(TipMask::new(4, 1, vec![255; 4]).expect("tip")));
    h.stamp(&[dab(32.0, 32.0, 12.0, 1.0)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);

    // Radius 12: the stamp spans 20..44 across and 29..35 down.
    assert_eq!(h.pixel(21, 32)[3], 255, "the long axis should reach 20");
    assert_eq!(h.pixel(43, 32)[3], 255, "and 44");
    assert_eq!(h.pixel(32, 30)[3], 255, "the short axis is 3 px each way");
    assert_eq!(
        h.pixel(32, 38)[3],
        0,
        "6 px down is past a quarter-height stamp — it is being stretched"
    );

    // Turned on its side, the same file must give the same picture rotated.
    h.set_tip(Some(TipMask::new(1, 4, vec![255; 4]).expect("tip")));
    h.stamp(&[dab(32.0, 32.0, 12.0, 1.0)]);
    h.commit_to(1, Color::WHITE, 1.0, BrushMode::Paint);
    assert_eq!(h.pixel_in(1, 32, 21)[3], 255, "portrait reaches down");
    assert_eq!(
        h.pixel_in(1, 38, 32)[3],
        0,
        "and not across — the aspect is being applied to the wrong axis"
    );
}

/// Paint one straight stroke of `brush` into `slot` and leave it committed.
///
/// Driven from a `Brush` through a real `StrokeBuilder` rather than from
/// hand-built dabs, because the claim being tested spans the whole chain: the
/// brush says the dab may turn, the builder works out the angle from the
/// heading it is travelling on, and the vertex shader builds the quad — and the
/// tip with it — rotated.
fn stroke_into(h: &mut Harness, brush: Brush, slot: u32, from: Vec2, to: Vec2) {
    let mut s = StrokeBuilder::new();
    s.begin(brush, [1.0, 1.0, 1.0], InputPoint::new(from, 1.0, 0.0));
    s.extend(InputPoint::new(to, 1.0, 0.1));
    let dabs: Vec<Dab> = s.drain_pending().collect();
    h.stamp(&dabs);
    h.commit_to(slot, Color::WHITE, 1.0, BrushMode::Paint);
}

/// A 4:1 landscape mask, solid: a bar 24 px long and 6 px tall at radius 12.
///
/// Deliberately the *only* asymmetry in the brushes below — `dab_ratio` stays
/// at 1.0, so a round dab would paint the same disc whichever way it was
/// turned, and every one of these assertions is about the bitmap.
fn bar_tip() -> TipMask {
    TipMask::new(4, 1, vec![255; 4]).expect("tip")
}

#[test]
fn a_stamp_turns_to_follow_the_stroke() {
    // The brush the whole "does a custom tip follow the stroke" question is
    // about. A bitmap is not rotationally symmetric, so `dab_angle_follows_
    // stroke` is live for a stamp whatever its roundness — and the mask has to
    // turn with the quad, not merely be sampled inside a quad that turned.
    //
    // Asserted as the *width of the mark across the line of travel*: a bar
    // dragged along its own length leaves a band as narrow as the mask is tall,
    // and one dragged sideways leaves a band as wide as the mask is long. Those
    // are 6 px and 24 px, so 10 px off the line tells them apart with room to
    // spare either way.
    let mut h = harness_or_skip!();
    h.set_tip(Some(bar_tip()));

    let rake = Brush {
        size: 24.0,
        spacing: 0.25,
        stabilization: 0.0,
        pressure_size: false,
        dab_angle_follows_stroke: true,
        ..Default::default()
    };

    // Travelling down the canvas. The first dab of a stroke is laid before any
    // direction exists and points along +x, so the far end of the stroke is
    // where the heading has taken effect.
    stroke_into(
        &mut h,
        rake,
        0,
        Vec2::new(32.0, 16.0),
        Vec2::new(32.0, 48.0),
    );
    assert_eq!(h.pixel_in(0, 32, 40)[3], 255, "the stroke should paint");
    assert_eq!(
        h.pixel_in(0, 42, 40)[3],
        0,
        "10 px to the side of a downward stroke: the stamp did not turn with it"
    );

    // The same brush pulled the other way. Nothing about the mask changed, so
    // if the mark is narrow this time as well it is the *stroke* deciding.
    stroke_into(
        &mut h,
        rake,
        1,
        Vec2::new(16.0, 32.0),
        Vec2::new(48.0, 32.0),
    );
    assert_eq!(h.pixel_in(1, 40, 32)[3], 255, "the stroke should paint");
    assert_eq!(
        h.pixel_in(1, 40, 42)[3],
        0,
        "10 px below a rightward stroke should be clear"
    );

    // The control, and the reason the first assertion means anything: with the
    // dab held at a fixed angle the downward stroke really does reach 10 px to
    // the side, because the bar is lying across it.
    let nib = Brush {
        dab_angle_follows_stroke: false,
        ..rake
    };
    stroke_into(&mut h, nib, 2, Vec2::new(32.0, 16.0), Vec2::new(32.0, 48.0));
    assert_eq!(
        h.pixel_in(2, 42, 40)[3],
        255,
        "a fixed-angle stamp should lie across the downward stroke"
    );
}

#[test]
fn a_stamp_rolls_to_a_new_angle_on_every_dab() {
    // The third angle state, and the one a charcoal, a fringe or a grain brush
    // is made of. `a_jittered_angle_spreads_a_stroke_the_way_a_fixed_one_cannot`
    // makes this claim for an *elliptical* dab; this is the bitmap half of it,
    // with `dab_ratio` left at 1.0 so the quad is square and the only thing that
    // can reach off the line is the mask being turned.
    //
    // Without it a stamp repeated down a stroke lands the same way up every
    // time, which reads as machined ruling rather than as a loaded brush.
    let mut h = harness_or_skip!();
    h.set_tip(Some(bar_tip()));

    let comb = Brush {
        size: 24.0,
        spacing: 0.25,
        stabilization: 0.0,
        pressure_size: false,
        dab_ratio: 1.0,
        ..Default::default()
    };
    // 10 px above the line: outside the bar's 3 px half-height, well inside its
    // 12 px half-length.
    let reach = |h: &Harness, slot: u32| (16..48).map(|x| h.pixel_in(slot, x, 22)[3]).max();

    stroke_into(
        &mut h,
        comb,
        0,
        Vec2::new(14.0, 32.0),
        Vec2::new(50.0, 32.0),
    );
    assert_eq!(
        reach(&h, 0),
        Some(0),
        "stamps all lying along the stroke should not reach 10 px off it"
    );

    let charcoal = Brush {
        dab_angle_jitter: 360.0,
        ..comb
    };
    stroke_into(
        &mut h,
        charcoal,
        1,
        Vec2::new(14.0, 32.0),
        Vec2::new(50.0, 32.0),
    );
    assert!(
        reach(&h, 1).unwrap_or(0) > 0,
        "a stamp free to roll should reach out towards its long side"
    );
}

/// A rotated stamp must not lose its corners to the damaged rect.
///
/// The failure this guards has happened twice in this project and is nasty both
/// times: coverage outside the committed rectangle stays in the scratch,
/// redraws as a live preview so the stroke appears to hang, and is then baked
/// in by the *next* stroke wearing that stroke's colour.
///
/// A round dab fits inside its bounding square at any angle, so the old
/// circumscribing-circle bound held for every brush that existed. A bitmap tip
/// paints right into its quad's corners, and a quad turned 45° reaches out to
/// `radius * sqrt(2)`.
#[test]
fn a_rotated_stamp_is_committed_all_the_way_into_its_corners() {
    let mut h = harness_or_skip!();

    let brush = Brush {
        size: 24.0,
        spacing: 1.0,
        stabilization: 0.0,
        pressure_size: false,
        dab_angle: 45.0,
        // A round dab has no angle, so the ratio has to say the dab is shaped
        // before `dab_angle` means anything to the bounds.
        dab_ratio: 1.0,
        ..Default::default()
    };

    let mut s = StrokeBuilder::new();
    s.begin(
        brush,
        [1.0, 1.0, 1.0],
        InputPoint::new(Vec2::new(32.0, 32.0), 1.0, 0.0),
    );
    let bounds = s.bounds();
    let reach = 12.0 * std::f32::consts::SQRT_2;
    assert!(
        bounds.min.x <= 32.0 - reach + 0.01 && bounds.max.y >= 32.0 + reach - 0.01,
        "the damaged rect stops short of a 45° quad's corners: {bounds:?}"
    );

    // And the whole stamp really is committed: a solid tip turned 45° must mark
    // the far corner of its own footprint.
    h.set_tip(Some(TipMask::new(2, 2, vec![255; 4]).expect("tip")));
    let dabs: Vec<Dab> = s.drain_pending().collect();
    h.stamp(&dabs);
    let rect = bounds.to_pixels_clamped(UVec2::splat(DOC)).expect("rect");
    let mut enc = h.encoder();
    h.canvas.commit_stroke(
        &h.gpu.device,
        &h.gpu.queue,
        &mut enc,
        0,
        rect,
        &[rect],
        StrokeStyle {
            color: Color::WHITE,
            ..Default::default()
        },
    );
    h.gpu.queue.submit(Some(enc.finish()));

    // 45°, so the quad's corner sits on the +y axis at radius * sqrt(2) ≈ 17.
    assert_eq!(
        h.pixel(32, 47)[3],
        255,
        "the corner of the rotated stamp was outside the committed rect"
    );
}

/// The aspect twin of the test above, and the pair is the point.
///
/// That one pins the *angle* half of the rule: a turned quad reaches into
/// corners a circle does not. `stroke.rs`'s
/// `the_damaged_box_covers_a_dab_a_ratio_modulation_fattened` pins the
/// *arithmetic*: that `StrokeBuilder`'s box covers the quad implied by each
/// dab's own `radius`, `aspect` and `angle`. Neither can see the thing that
/// actually broke, which was a disagreement between that implied quad and the
/// one `dab.wgsl` rasterises, so that bridge is what needs a device and is the
/// whole of what this test is for.
///
/// **It does not also exercise the cell mask, and the pieces below are not
/// evidence that it does.** `DOC` is 64 and `damage::TILE` is 64, so every
/// stroke in this file marks the single cell `(0, 0)` and
/// `TileMask::pieces(rect)` provably returns `vec![rect]` — byte for byte what
/// the neighbouring test writes out by hand. They are read from the real mask
/// so this test commits by the route `finish_stroke` uses rather than a
/// reconstruction of it, which is worth having and is *all* it is worth here.
/// The mask half of "feed both or neither" is
/// `the_cell_mask_covers_a_dab_a_ratio_modulation_fattened`, on a 512 canvas,
/// and only there.
///
/// The brush is `mypaint/dieterle/arrow-1`'s shape: `dab_ratio` 10.0 against a
/// `Ratio` modulation reaching -9.0, so `aspect` is 1.0 and the dab is *round*
/// while the nominal ratio still says its short semi-axis is a tenth of the
/// long one. Turned 45°, which is what puts the shortfall on **both** axes: a
/// round dab is unchanged by the rotation but its box is not, and the nominal
/// reading gives a half-extent of 9.3 px where the dab reaches 12.
#[test]
fn a_stamp_a_ratio_modulation_rounded_out_is_committed_across_its_short_axis() {
    let mut h = harness_or_skip!();

    let brush = Brush {
        size: 24.0,
        spacing: 1.0,
        stabilization: 0.0,
        pressure_size: false,
        // Hard, so the coverage at the pixel asserted on below is only ever 0
        // or 1 and comparing an exact byte is legitimate — the rule
        // `a_hard_edged_rectangular_lift_is_exact` follows. One level of slack
        // is allowed anyway, because the discrimination wanted here is 0
        // against 255 and nothing is bought by insisting on the last bit.
        hardness: 1.0,
        dab_ratio: 10.0,
        // 45°, so the nominal reading is short on x and y alike and both are
        // asserted below. A round dab paints the same disc at any angle, but
        // its *box* does not: `|r cos| + |short sin|` is 9.3 px off the nominal
        // short axis and 17.0 off the real one.
        dab_angle: 45.0,
        modulations: [Modulation {
            target: DabTarget::Ratio,
            input: DabInput::Stroke,
            low: -9.0,
            high: 0.0,
            curve: ResponseCurve::LINEAR,
        }]
        .into_iter()
        .collect(),
        ..Default::default()
    };

    // A tap, at the head of the stroke where the `Stroke` input reads zero.
    let mut s = StrokeBuilder::new();
    s.begin(
        brush,
        [1.0, 1.0, 1.0],
        InputPoint::new(Vec2::new(32.0, 32.0), 1.0, 0.0),
    );
    let bounds = s.bounds();
    let dabs: Vec<Dab> = s.drain_pending().collect();
    assert_eq!(dabs.len(), 1);
    // Says the premise held rather than assuming it: if the modulation stopped
    // rounding the dab out, everything below would pass while testing nothing.
    assert!(
        (dabs[0].aspect - 1.0).abs() < 1e-3,
        "the dab was not rounded out, so nothing is being tested: aspect {}",
        dabs[0].aspect
    );

    h.stamp(&dabs);
    let rect = bounds.to_pixels_clamped(UVec2::splat(DOC)).expect("rect");
    // The real pieces, off the stroke's own mask — the same ones `finish_stroke`
    // hands to both the undo capture and the commit.
    let pieces = s.damage().pieces(rect);
    let mut enc = h.encoder();
    h.canvas.commit_stroke(
        &h.gpu.device,
        &h.gpu.queue,
        &mut enc,
        0,
        rect,
        &pieces,
        StrokeStyle {
            color: Color::WHITE,
            ..Default::default()
        },
    );
    h.gpu.queue.submit(Some(enc.finish()));

    // Ten and a half pixels from the centre of a round dab of radius 12, on
    // each axis in turn. Both sit inside the disc the shader paints — the
    // sample is at 0.876 of the radius, inside `smoothstep`'s inner edge of
    // 0.917, so coverage there is exactly 1 — and both sit outside the 9.3 px
    // half-extent the nominal ratio described, which reached only pixel 41.
    //
    // Both axes, because the nominal reading was short on each: y is the axis
    // the ratio squashes and x is the one the 45° rotation carries it onto.
    for (x, y) in [(32, 42), (42, 32)] {
        let alpha = h.pixel(x, y)[3];
        assert!(
            alpha >= 254,
            "({x}, {y}) is inside the dab and was left out of the committed \
             pieces: alpha {alpha}"
        );
    }
}

#[test]
fn clearing_the_tip_restores_the_round_brush() {
    // Tips are per stroke, so going back to a round brush has to actually go
    // back — a stale `use_tip` flag would leave every later stroke square.
    let mut h = harness_or_skip!();

    let tip = TipMask::new(1, 1, vec![255]).expect("tip");
    h.set_tip(Some(tip));
    assert!(h.canvas.has_tip());

    h.set_tip(None);
    assert!(!h.canvas.has_tip());

    h.stamp(&[dab(32.0, 32.0, 12.0, 1.0)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);

    assert_eq!(h.pixel(32, 32)[3], 255, "the centre should still paint");
    assert_eq!(
        h.pixel(CORNER, CORNER)[3],
        0,
        "the corner should be round again"
    );
}

#[test]
fn a_second_brush_with_a_different_tip_replaces_the_first() {
    // `set_tip` skips the upload when the mask is the same allocation as last
    // time, which is what keeps a stroke's first frame off the texture-upload
    // path. The failure mode of that guard is a *stale* tip, so this stamps two
    // masks that disagree about which half paints and checks the second won.
    let mut h = harness_or_skip!();

    h.set_tip(Some(TipMask::new(2, 1, vec![255, 0]).expect("tip")));
    h.stamp(&[dab(32.0, 32.0, 12.0, 1.0)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);
    assert_eq!(h.pixel(22, 32)[3], 255);
    assert_eq!(h.pixel(42, 32)[3], 0);

    h.set_tip(Some(TipMask::new(2, 1, vec![0, 255]).expect("tip")));
    h.stamp(&[dab(32.0, 32.0, 12.0, 1.0)]);
    h.commit_to(1, Color::WHITE, 1.0, BrushMode::Paint);
    assert_eq!(
        h.pixel_in(1, 22, 32)[3],
        0,
        "the second tip's empty half painted — the first tip is still bound"
    );
    assert_eq!(h.pixel_in(1, 42, 32)[3], 255);
}

// ---------------------------------------------------------------------------
// Coloured stamps
// ---------------------------------------------------------------------------

/// A two-texel stamp: red on the left, blue on the right, solid throughout.
///
/// Two colours rather than one, because a stamp that put down a single colour
/// could be passing it through the *dab*'s colour rather than the tip's and
/// nothing here would know. Sampled at x = 22 and x = 42, which is where
/// `a_second_brush_with_a_different_tip_replaces_the_first` already establishes
/// a 2x1 tip's halves land.
fn two_colour_tip() -> TipMask {
    TipMask::coloured(2, 1, vec![255, 255], vec![255, 0, 0, 0, 0, 255]).expect("tip")
}

#[test]
fn a_coloured_stamp_puts_down_its_own_colour_and_not_the_palettes() {
    // The whole feature in two assertions. The mark is red on one side and blue
    // on the other, which no single source of colour could produce: a stamp
    // whose colour never reached the scratch — the flag unset, the texture
    // unbound, the un-premultiply wrong — falls back to `Dab::color`, which the
    // harness's `dab()` leaves at zero, so both halves come out black.
    let mut h = harness_or_skip!();

    h.set_tip(Some(two_colour_tip()));
    assert!(h.canvas.stamps_tip_color());
    h.stamp_colored(&[dab(32.0, 32.0, 12.0, 1.0)], true);
    h.commit(Color::from_srgb_u8(0, 255, 0, 255), 1.0, BrushMode::Paint);

    // The tip's colour is straight sRGB and the layer stores sRGB, so a solid
    // texel comes back as the byte that went in.
    assert_near(h.pixel(22, 32), [255, 0, 0], 4, "the stamp's left half");
    assert_near(h.pixel(42, 32), [0, 0, 255], 4, "the stamp's right half");
}

/// **The test a coloured stamp hangs on**, and the reason this feature could be
/// built inside the existing pipelines at all.
///
/// `composite.wgsl` draws the stroke as a live preview and `commit.wgsl`
/// replaces it at pointer-up, and the two must agree or the mark jumps under the
/// artist's hand. Neither was touched for this: the stamp's colour goes into the
/// same scratch a smudging brush writes, so both read it through the code they
/// already shared. That is the claim, and this is what would catch it being
/// false — a preview reading the palette where the commit read the stamp is a
/// mark that turns a different colour on release.
///
/// Across the blended commits as well as Normal, because `fs_blend` is a second
/// entry point that could have been given the colour differently.
#[test]
fn a_coloured_stamps_preview_and_its_commit_agree() {
    let mut h = harness_or_skip!();

    let under = [70u8, 160, 110, 255];
    // Deliberately not the stamp's own colours, so a preview or a commit reading
    // the wrong source of the two is a visible difference rather than a
    // coincidence.
    let palette = Color::from_srgb_u8(20, 20, 240, 255);

    for blend in [BlendMode::Normal].into_iter().chain(BRUSH_BLENDS) {
        reset(&mut h);
        let rect = whole(&h);
        h.write_block(0, rect, under);
        h.set_tip(Some(two_colour_tip()));
        h.stamp_colored(&[dab(32.0, 32.0, 16.0, 1.0)], true);

        let stack = [layer(0, 1.0, BlendMode::Normal)];
        let style = StrokeStyle {
            color: palette,
            opacity: 1.0,
            mode: BrushMode::Paint,
            blend,
            per_dab_color: true,
            on_mask: false,
        };
        // Both halves of the stamp, because a divergence could be in the
        // un-premultiply and show up on one colour and not the other.
        for x in [22, 42] {
            let previewed = h.composite_pixel_with(&stack, style, x, 32);
            h.commit_blended_to(0, palette, 1.0, BrushMode::Paint, blend);
            let committed = h.composite_pixel(&stack, x, 32);
            assert_near(
                committed,
                [previewed[0], previewed[1], previewed[2]],
                2,
                &format!("{blend:?} jumped at pointer-up under a coloured stamp at x={x}"),
            );
            // The commit consumed the scratch, so put it back for the second
            // sample rather than measuring an empty one.
            h.stamp_colored(&[dab(32.0, 32.0, 16.0, 1.0)], true);
        }
    }
}

#[test]
fn a_tip_with_no_colour_is_the_exact_identity_on_the_colour_path() {
    // The rule the whole shape of this rests on: a brush with no coloured stamp
    // must pay nothing and change by nothing. `dab_rgb` returns `in.color`
    // through a `select` when `use_tip_color` is zero, so a smudging stroke
    // through a plain tip has to deposit precisely what it deposited before
    // coloured stamps existed — and this compares against a stroke with no tip
    // bound at all, at the centre where both are at full coverage.
    let mut h = harness_or_skip!();

    let picked = [0.85f32, 0.25, 0.55];

    h.stamp_colored(&[coloured_dab(32.0, 32.0, 12.0, 1.0, picked)], true);
    h.commit(Color::BLACK, 1.0, BrushMode::Paint);
    let round = h.pixel(32, 32);

    h.set_tip(Some(TipMask::new(2, 2, vec![255; 4]).expect("tip")));
    assert!(!h.canvas.stamps_tip_color());
    h.stamp_colored(&[coloured_dab(32.0, 32.0, 12.0, 1.0, picked)], true);
    h.commit_to(1, Color::BLACK, 1.0, BrushMode::Paint);

    assert_eq!(
        h.pixel_in(1, 32, 32),
        round,
        "a tip with no colour moved what the dab deposited"
    );
}

#[test]
fn a_coloured_stamp_stops_being_one_when_the_tip_is_taken_off() {
    // Tips are per stroke, so going back has to actually go back, exactly as
    // `use_tip` does.
    //
    // What this pins is the *flag*, and the pixels beneath it are a weaker
    // check than they look: the placeholder is a fresh zeroed texture, so a
    // stale flag would read an alpha of nothing and `dab_rgb` would fall back
    // to the dab's colour anyway. That fallback is deliberate — a stale flag is
    // benign by construction rather than by luck — and
    // `a_stamp_told_not_to_colour_paints_what_the_dab_carried` is what reads
    // the shader half against a texture that really does hold a colour.
    let mut h = harness_or_skip!();

    h.set_tip(Some(two_colour_tip()));
    assert!(h.canvas.stamps_tip_color());

    h.set_tip(Some(TipMask::new(2, 1, vec![255, 255]).expect("tip")));
    assert!(!h.canvas.stamps_tip_color());
    h.stamp_colored(
        &[coloured_dab(32.0, 32.0, 12.0, 1.0, [0.0, 1.0, 0.0])],
        true,
    );
    h.commit(Color::BLACK, 1.0, BrushMode::Paint);
    assert_near(h.pixel(22, 32), [0, 255, 0], 4, "the stamp is still red");

    h.set_tip(None);
    assert!(!h.canvas.stamps_tip_color());
}

#[test]
fn a_coloured_stamp_still_does_not_compound() {
    // The wet-layer guarantee, through the third source of per-dab colour. The
    // stamp modulates *coverage* exactly as a plain tip does and its colour
    // rides in the second attachment, so a stroke crossing itself is no more
    // opaque than one that does not — which is the thing that would break if the
    // colour had been folded into the coverage instead.
    let mut h = harness_or_skip!();

    h.set_tip(Some(two_colour_tip()));
    let d = dab(32.0, 32.0, 12.0, 0.5);
    h.stamp_colored(&[d, d, d, d], true);
    h.commit(Color::BLACK, 1.0, BrushMode::Paint);

    let px = h.pixel(22, 32);
    assert!(
        px[3].abs_diff(128) <= 2,
        "four half-coverage stamps compounded to {}",
        px[3]
    );
    // And still the stamp's own red. Not compared against 255: the layer holds
    // colour *premultiplied*, so half-covered red is stored as the sRGB encoding
    // of a linear half — which is why the two channels that must be nothing are
    // what this asserts on.
    assert!(
        px[0] > 150 && px[1] == 0 && px[2] == 0,
        "the stamp's colour did not survive the overlap: {px:?}"
    );
}

/// A stamp whose edge is soft has to keep the colour it was drawn in there too.
///
/// **This is what the premultiply on upload is for**, and the fixture is built
/// so that leaving it out actually shows: the texels the stamp does not cover
/// hold *blue*. Premultiplied, a texel with no coverage contributes `(0,0,0,0)`
/// to the filter and the un-premultiply divides by the alpha that is really
/// there, so red survives all the way out to the edge. Filtering **straight**
/// colour would blend that blue in by area — the classic halo — and the fade
/// would come back purple.
///
/// The first draft of this test coloured every texel red, which made the green
/// and blue channels structurally zero: it passed with the premultiply deleted
/// entirely and was therefore not a test at all.
#[test]
fn a_soft_edged_colour_stamp_does_not_halo() {
    let mut h = harness_or_skip!();

    // A 4x1 stamp: solid red, fading out, and blue where nothing is stamped.
    let tip = TipMask::coloured(
        4,
        1,
        vec![255, 255, 128, 0],
        vec![255, 0, 0, 255, 0, 0, 255, 0, 0, 0, 0, 255],
    )
    .expect("tip");
    h.set_tip(Some(tip));
    h.stamp_colored(&[dab(32.0, 32.0, 16.0, 1.0)], true);
    h.commit(Color::BLACK, 1.0, BrushMode::Paint);

    // Walk out through the fade. Colour is only asserted where there is enough
    // alpha for it to mean anything — below a level or two the stored byte moves
    // by six for one level of alpha, which says nothing about this code.
    let mut checked = 0;
    for x in 26..40 {
        let px = h.pixel(x, 32);
        if px[3] < 40 {
            continue;
        }
        checked += 1;
        assert!(
            px[2] < 12,
            "the stamp haloed at x={x}: {px:?} has the uncovered blue in it"
        );
    }
    assert!(
        checked > 2,
        "the fade was never sampled — only {checked} px"
    );
}

/// The colour is refused where there is nowhere for it to land, and the
/// refusal has to reach the **dab pass**, not only the pipeline choice.
///
/// `StrokeStyle::per_dab_color` turns on for a smudging brush as well as for a
/// coloured stamp, so a brush that is both would take the coloured pipeline for
/// its own reason — and if `set_tip` decided the stamp's colour by itself, it
/// would go on stamping it into a mask that previews grey and commits red. One
/// argument, decided once by the caller, is what stops that; this is the guard.
#[test]
fn a_stamp_told_not_to_colour_paints_what_the_dab_carried() {
    let mut h = harness_or_skip!();

    let picked = [0.0f32, 1.0, 0.0];

    // The same two-colour stamp, its colour refused. What lands is the dab's
    // own colour — a smudging brush's pickup, here — and not the stamp's red.
    h.set_tip_without_colour(Some(two_colour_tip()));
    assert!(!h.canvas.stamps_tip_color());
    h.stamp_colored(&[coloured_dab(32.0, 32.0, 12.0, 1.0, picked)], true);
    h.commit(Color::BLACK, 1.0, BrushMode::Paint);
    assert_near(h.pixel(22, 32), [0, 255, 0], 4, "the stamp coloured anyway");

    // And the same mask, offered, does stamp its own — so the refusal is the
    // argument rather than something about this tip.
    h.set_tip(Some(two_colour_tip()));
    assert!(h.canvas.stamps_tip_color());
    h.stamp_colored(&[coloured_dab(32.0, 32.0, 12.0, 1.0, picked)], true);
    h.commit_to(1, Color::BLACK, 1.0, BrushMode::Paint);
    assert_near(h.pixel_in(1, 22, 32), [255, 0, 0], 4, "the stamp's own red");
}

/// The same brush coming back with the answer changed has to be noticed.
///
/// `set_tip` early-outs on `Arc` identity, which is what keeps a texture upload
/// off the first frame of every stroke — and picking up the eraser does not
/// change the tip. So the early-out has to test the *decision* as well as the
/// mask, or a coloured stamp goes on colouring after the caller refused it.
#[test]
fn refusing_a_stamps_colour_is_noticed_even_though_the_tip_did_not_change() {
    let mut h = harness_or_skip!();

    let mask = Arc::new(two_colour_tip());
    h.canvas
        .set_tip(&h.gpu.device, &h.gpu.queue, Some(Arc::clone(&mask)), true);
    assert!(h.canvas.stamps_tip_color());

    // The very same allocation, with the colour now refused.
    h.canvas
        .set_tip(&h.gpu.device, &h.gpu.queue, Some(Arc::clone(&mask)), false);
    assert!(
        !h.canvas.stamps_tip_color(),
        "the early-out kept the stamp colouring after it was refused"
    );

    // And back again, which is the direction an eraser put down would take.
    h.canvas
        .set_tip(&h.gpu.device, &h.gpu.queue, Some(mask), true);
    assert!(h.canvas.stamps_tip_color());
}

// ---------------------------------------------------------------------------
// Paper grain
// ---------------------------------------------------------------------------

#[test]
fn grain_off_is_the_exact_identity() {
    // The claim the whole design rests on: a brush that asks for no paper must
    // paint precisely what it painted before the paper existed. The shader
    // computes `mix(1.0, tile, strength)`, so at strength zero the tile is
    // multiplied out exactly rather than nearly — and this binds a *black* tile
    // to prove the multiply really is by one and not by something close to it.
    let mut h = harness_or_skip!();

    h.stamp(&[dab(32.0, 32.0, 12.0, 1.0)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);
    let plain = h.pixel(32, 32);

    h.set_grain(Some((
        TipMask::new(2, 2, vec![0; 4]).expect("tile"),
        0.0,
        64.0,
    )));
    h.stamp(&[dab(32.0, 32.0, 12.0, 1.0)]);
    h.commit_to(1, Color::WHITE, 1.0, BrushMode::Paint);

    assert_eq!(
        h.pixel_in(1, 32, 32),
        plain,
        "a black paper at zero strength changed the mark"
    );
}

#[test]
fn grain_bites_coverage_out_of_a_dab() {
    // A tile that is solid on its left half and empty on its right, one tile per
    // 32 document pixels. At full strength the empty half must take the paint
    // away entirely and the solid half must leave it alone.
    //
    // Eight texels rather than two: the sampler filters linearly, so a two-texel
    // tile is a gradient from end to end with no flat region to assert on. Four
    // solid texels give a plateau over u = 1/16 .. 7/16, which at this scale is
    // document x 34..46 in each tile.
    let mut h = harness_or_skip!();

    let tile = TipMask::new(8, 1, vec![255, 255, 255, 255, 0, 0, 0, 0]).expect("tile");
    h.set_grain(Some((tile, 1.0, 32.0)));
    h.stamp(&[dab(48.0, 32.0, 20.0, 1.0)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);

    assert_eq!(
        h.pixel(40, 32)[3],
        255,
        "the paper's solid half should paint"
    );
    assert_eq!(h.pixel(56, 32)[3], 0, "its empty half should not");
}

#[test]
fn grain_is_anchored_to_the_paper_and_not_to_the_dab() {
    // What makes it *paper*. Two dabs at different places must land on different
    // parts of the tile, so the same brush pulled across a sheet catches and
    // skips rather than stamping the same texture over and over.
    //
    // Tile of 32 document pixels, solid over x = 34..46 of each tile and empty
    // over 50..62. Two identical dabs, one sitting in each: if the grain were
    // anchored to the dab, they would be indistinguishable.
    let mut h = harness_or_skip!();

    let tile = TipMask::new(8, 1, vec![255, 255, 255, 255, 0, 0, 0, 0]).expect("tile");
    h.set_grain(Some((tile, 1.0, 32.0)));
    h.stamp(&[dab(40.0, 32.0, 5.0, 1.0), dab(56.0, 32.0, 5.0, 1.0)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);

    assert_eq!(
        h.pixel(40, 32)[3],
        255,
        "a dab over the tile's solid stretch should paint"
    );
    assert_eq!(
        h.pixel(56, 32)[3],
        0,
        "an identical dab half a tile along should not — the grain is travelling \
         with the brush"
    );
}

#[test]
fn a_grained_stroke_still_saturates_under_overlap() {
    // The wet-layer guarantee has to survive the paper, exactly as it survived
    // the tip. Grain modulates coverage; it does not touch the blend state.
    let mut h = harness_or_skip!();

    let tile = TipMask::new(1, 1, vec![255]).expect("tile");
    h.set_grain(Some((tile, 1.0, 32.0)));
    h.stamp(&[dab(32.0, 32.0, 12.0, 0.5), dab(32.0, 32.0, 12.0, 0.5)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);

    let alpha = h.pixel(32, 32)[3];
    assert!(
        (100..=155).contains(&alpha),
        "expected ~128 (single coverage), got {alpha} — grained dabs are compounding"
    );
}

#[test]
fn a_grained_stroke_may_still_build_up() {
    // The two combine, and they have to: paper is what makes a build-up brush
    // interesting. A half-strength paper bites each dab down to 0.5 and the
    // stroke then composites towards solid, where a `max` would stop at 0.5.
    let mut h = harness_or_skip!();

    let tile = TipMask::new(1, 1, vec![128]).expect("tile");
    h.set_grain(Some((tile, 1.0, 32.0)));

    let d = dab(32.0, 32.0, 12.0, 1.0);
    h.stamp(&[d; 8]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);
    assert!(
        h.pixel(32, 32)[3].abs_diff(128) <= 4,
        "a max stroke should stop at the paper's own value, got {}",
        h.pixel(32, 32)[3]
    );

    h.stamp_building(&[d; 8]);
    h.commit_to(1, Color::WHITE, 1.0, BrushMode::Paint);
    assert!(
        h.pixel_in(1, 32, 32)[3] >= 250,
        "eight dabs through half-strength paper should build to solid, got {}",
        h.pixel_in(1, 32, 32)[3]
    );
}

// ---------------------------------------------------------------------------
// Selections
// ---------------------------------------------------------------------------

/// The left half of the canvas, as a rectangle selection.
fn left_half() -> Selection {
    Selection::rectangle(
        Vec2::new(0.0, 0.0),
        Vec2::new(DOC as f32 * 0.5, DOC as f32),
        UVec2::splat(DOC),
    )
    .expect("a selection")
}

#[test]
fn a_stroke_is_clipped_to_the_selection() {
    // The one that matters. A dab straddling the boundary must mark the layer
    // on one side of it and leave the other exactly as it was.
    //
    // The clip is applied in the *dab pass*, so what reaches the scratch is
    // already clipped and neither `composite.wgsl` nor `commit.wgsl` knows
    // there is a selection at all. That is deliberate: those two implement the
    // same blending maths, and a stroke clipped in one of them and not the
    // other would visibly jump at pointer-up.
    let mut h = harness_or_skip!();

    h.set_selection(Some(left_half()));
    h.stamp(&[dab(32.0, 32.0, 20.0, 1.0)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);

    assert_eq!(h.pixel(20, 32)[3], 255, "inside the selection");
    assert_eq!(h.pixel(44, 32)[3], 0, "outside it");
}

#[test]
fn no_selection_is_the_exact_identity() {
    // The claim the design rests on, the same one `grain_off_is_the_exact_
    // identity` makes: a document with nothing selected must paint precisely
    // what it painted before selections existed. The shader multiplies coverage
    // by a `select`ed 1.0 rather than branching, so this is a multiply by one.
    let mut h = harness_or_skip!();

    h.stamp(&[dab(32.0, 32.0, 12.0, 0.6)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);
    let plain = h.pixel(32, 32);

    // Set one and take it off again, so the placeholder path has really been
    // through a real mask rather than never having left its initial state.
    h.set_selection(Some(left_half()));
    h.set_selection(None);
    h.stamp(&[dab(32.0, 32.0, 12.0, 0.6)]);
    h.commit_to(1, Color::WHITE, 1.0, BrushMode::Paint);

    assert_eq!(
        h.pixel_in(1, 32, 32),
        plain,
        "clearing the selection did not restore the unclipped mark"
    );
}

#[test]
fn erasing_is_clipped_to_the_selection_too() {
    // The eraser is a blend state, not a shader branch, and the selection sits
    // upstream of both — so this cannot be got right for paint and wrong for
    // erase. Worth pinning anyway: an eraser that ignored the selection would
    // be the most destructive way for this to fail.
    let mut h = harness_or_skip!();

    h.fill(0, Color::WHITE);
    h.set_selection(Some(left_half()));
    h.stamp(&[dab(32.0, 32.0, 20.0, 1.0)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Erase);

    assert_eq!(h.pixel(20, 32)[3], 0, "erased inside the selection");
    assert_eq!(h.pixel(44, 32)[3], 255, "untouched outside it");
}

#[test]
fn a_selection_edge_is_antialiased() {
    // The reason coverage is a byte rather than a bit. The boundary is put down
    // the middle of a column, so that column must come out about half painted —
    // a hard edge here is a staircase the artist can see, at every zoom level.
    let mut h = harness_or_skip!();

    let sel = Selection::rectangle(
        Vec2::new(0.0, 0.0),
        Vec2::new(32.5, DOC as f32),
        UVec2::splat(DOC),
    )
    .expect("a selection");
    h.set_selection(Some(sel));
    h.stamp(&[dab(32.0, 32.0, 20.0, 1.0)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);

    let edge = h.pixel(32, 32)[3];
    assert!(
        (100..=155).contains(&edge),
        "expected ~128 on the half-covered column, got {edge}"
    );
    assert_eq!(h.pixel(31, 32)[3], 255, "the column before it is fully in");
    assert_eq!(h.pixel(33, 32)[3], 0, "the one after is fully out");
}

#[test]
fn a_clipped_stroke_still_saturates_under_overlap() {
    // The wet-layer guarantee has to survive the selection exactly as it
    // survived the tip and the paper. The mask modulates coverage; it does not
    // touch the blend state.
    let mut h = harness_or_skip!();

    h.set_selection(Some(left_half()));
    h.stamp(&[dab(20.0, 32.0, 12.0, 0.5), dab(20.0, 32.0, 12.0, 0.5)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);

    let alpha = h.pixel(20, 32)[3];
    assert!(
        (100..=155).contains(&alpha),
        "expected ~128 (single coverage), got {alpha} — clipped dabs are compounding"
    );
}

#[test]
fn nothing_outside_a_selections_own_rectangle_is_paintable() {
    // The mask covers only its own bounds, so everywhere else has to be decided
    // by the shader rather than by a texture lookup. Clamping instead of
    // rejecting would leave the whole row and column beyond a rectangle
    // selection paintable — the boundary texels smeared across the canvas.
    let mut h = harness_or_skip!();

    let sel = Selection::rectangle(
        Vec2::new(24.0, 24.0),
        Vec2::new(40.0, 40.0),
        UVec2::splat(DOC),
    )
    .expect("a selection");
    h.set_selection(Some(sel));
    // Wide enough to reach well past the selection on every side.
    h.stamp(&[dab(32.0, 32.0, 28.0, 1.0)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);

    assert_eq!(h.pixel(32, 32)[3], 255, "inside");
    for (x, y, where_) in [
        (32, 12, "above"),
        (32, 52, "below"),
        (12, 32, "left of"),
        (52, 32, "right of"),
    ] {
        assert_eq!(h.pixel(x, y)[3], 0, "paint landed {where_} the selection");
    }
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

    let enc = h.encoder();
    h.canvas.clear_layer(&h.gpu.queue, 0);
    h.gpu.queue.submit(Some(enc.finish()));
    assert_eq!(h.pixel(32, 32)[3], 0);

    h.canvas
        .write_layer_rect(&h.gpu.device, &h.gpu.queue, 0, rect, &saved);
    assert_eq!(h.pixel(32, 32)[3], 255, "undo restore lost the pixels");
}

/// A cut is a read, a `Clip::cut_from_layer` and a write, and this is the whole
/// of it end to end: what stays on the layer has to be exactly what did not go
/// on the clipboard.
///
/// The complement itself is arithmetic and is pinned without a device in
/// `umber_core::clipboard`. What only this can show is that the arithmetic
/// survives the round trip through the texture — the layer is `Rgba8UnormSrgb`
/// and the remainder is written as raw bytes, so a conversion creeping into
/// either direction would put a rim of coverage back on a canvas the artist cut
/// clean. That rim is the exact ghost outline a masked lift used to leave.
#[test]
fn a_cut_leaves_the_layer_holding_exactly_what_it_did_not_take() {
    let mut h = harness_or_skip!();

    let rect = PixelRect {
        x: 8,
        y: 8,
        width: 40,
        height: 40,
    };
    // A flat block, so every pixel starts at a known alpha and the only thing
    // that can vary across the rectangle is the selection's own coverage.
    h.write_block(0, rect, [200, 90, 30, 255]);

    // A triangle, so its edge cuts the pixel grid diagonally and the rectangle
    // holds fully covered, partly covered and untouched pixels at once.
    let selection = Selection::from_rings(
        vec![vec![
            Vec2::new(12.0, 12.0),
            Vec2::new(44.0, 20.0),
            Vec2::new(20.0, 44.0),
        ]],
        UVec2::splat(DOC),
    )
    .expect("a selection");

    let before = h
        .canvas
        .read_layer_rect(&h.gpu.device, &h.gpu.queue, 0, rect);
    let cut = umber_core::Clip::cut_from_layer(rect, &before, Some(&selection)).expect("a cut");
    h.canvas
        .write_layer_rect(&h.gpu.device, &h.gpu.queue, 0, rect, &cut.remainder);

    let after = h
        .canvas
        .read_layer_rect(&h.gpu.device, &h.gpu.queue, 0, rect);
    let mut cleared = 0;
    let mut partial = 0;
    for i in (0..before.len()).step_by(4) {
        assert_eq!(
            cut.clip.pixels()[i + 3] as u32 + after[i + 3] as u32,
            before[i + 3] as u32,
            "pixel {} does not add back up to what was there",
            i / 4
        );
        match after[i + 3] {
            0 => cleared += 1,
            a if a < before[i + 3] => partial += 1,
            _ => {}
        }
    }
    assert!(
        cleared > 0 && partial > 0,
        "the fixture found {cleared} cleared and {partial} partly cut pixels, \
         so it is not testing the edge"
    );
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
    // Growth reallocates the texture array and copies every page. Losing
    // artwork when the user adds a fifth layer would be unforgivable.
    //
    // The **atlas** is what grows; the page table is `MAX_SLOTS` deep from the
    // moment the store exists and is never grown, which is why the reading is
    // `page_count` and why `slot_capacity` is now a constant.
    let mut h = harness_or_skip!();

    h.fill(0, Color::from_srgb_u8(200, 40, 40, 255));
    assert!(h.canvas.page_count() < 8);

    h.canvas.ensure_pages(&h.gpu.device, &h.gpu.queue, None, 8);
    assert!(h.canvas.page_count() >= 8);

    assert_near(h.pixel_in(0, 32, 32), [200, 40, 40], 2, "after growth");
    assert_eq!(
        h.pixel_in(7, 32, 32)[3],
        0,
        "a slot nothing has painted on reads as transparent"
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

#[test]
fn export_flattens_to_straight_alpha() {
    // Also guards the uniform layout: the export flag pushed the View struct
    // past a 16-byte boundary once already, and a mismatch there is a
    // validation error rather than a wrong pixel.
    let mut h = harness_or_skip!();
    let ink = Color::from_srgb_u8(200, 120, 40, 255);
    h.fill(0, ink);

    let pixels = h.canvas.export_rgba(
        &h.gpu.device,
        &h.gpu.queue,
        &[layer(0, 1.0, BlendMode::Normal)],
    );
    assert_eq!(pixels.len(), (DOC * DOC * 4) as usize);

    let at = |x: u32, y: u32| {
        let i = ((y * DOC + x) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };

    let centre = at(32, 32);
    assert_eq!(centre[3], 255, "painted area should be opaque");
    assert_near(centre, [200, 120, 40], 3, "exported ink");
}

#[test]
fn export_leaves_unpainted_pixels_transparent() {
    // The screen composites over a checkerboard; the file must not bake that
    // in, or every export would come out on a grey plaid background.
    let mut h = harness_or_skip!();
    h.stamp(&[dab(32.0, 32.0, 6.0, 1.0)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);

    let pixels = h.canvas.export_rgba(
        &h.gpu.device,
        &h.gpu.queue,
        &[layer(0, 1.0, BlendMode::Normal)],
    );
    let corner = ((2 * DOC + 2) * 4) as usize;
    assert_eq!(pixels[corner + 3], 0, "corner should be transparent");
}

// ---------------------------------------------------------------------------
// Layer masks and clipping
// ---------------------------------------------------------------------------

/// Fill a slice with one exact colour, byte for byte.
///
/// Not `Harness::fill`, which stamps a dab and therefore has an antialiased
/// edge: a mask is read a channel at a time and a test of it wants to know
/// exactly what that channel holds.
fn fill_slot(h: &mut Harness, slot: u32, rgba: [u8; 4]) {
    let size = h.canvas.doc_size();
    h.write_block(
        slot,
        PixelRect {
            x: 0,
            y: 0,
            width: size.x,
            height: size.y,
        },
        rgba,
    );
}

#[test]
fn a_mask_hides_what_it_covers() {
    // The whole feature in one assertion: white reveals, black hides, and mid
    // grey is a partial. Split down the canvas so all three are read out of one
    // composite, which is also what proves the mask is sampled per fragment
    // rather than folded into the layer.
    let mut h = harness_or_skip!();
    h.canvas.ensure_slots(&h.gpu.device, &h.gpu.queue, 2);

    fill_slot(&mut h, 0, [255, 255, 255, 255]);
    // The mask: opaque, and grey where the layer should be dimmed. The
    // composite reads the red channel through the array's **raw** view, so the
    // byte over 255 is the multiplier and 128 is a half. It used to read
    // through the sRGB view, where the same half was 188 — which is what this
    // fixture carried, and 73 of the 256 multipliers were unreachable.
    fill_slot(&mut h, 1, [255, 255, 255, 255]);
    h.canvas.mark_mask_slot(1);
    h.canvas.write_layer_rect(
        &h.gpu.device,
        &h.gpu.queue,
        1,
        PixelRect {
            x: 0,
            y: 0,
            width: 20,
            height: DOC,
        },
        &[0u8, 0, 0, 255].repeat((20 * DOC) as usize),
    );
    h.canvas.write_layer_rect(
        &h.gpu.device,
        &h.gpu.queue,
        1,
        PixelRect {
            x: 20,
            y: 0,
            width: 20,
            height: DOC,
        },
        &[128u8, 128, 128, 255].repeat((20 * DOC) as usize),
    );

    let mut masked = layer(0, 1.0, BlendMode::Normal);
    masked.mask = Some(1);

    // Hidden: the checkerboard, exactly as `a_hidden_layer_contributes_nothing`
    // reads it.
    let px = h.composite_pixel(&[masked], 10, 32);
    assert!(
        px[0] > 190 && px[0] < 235,
        "a black mask must hide the layer, got {px:?}"
    );
    // Revealed: white, untouched.
    assert_near(
        h.composite_pixel(&[masked], 55, 32),
        [255, 255, 255],
        2,
        "a white mask must be the identity",
    );
    // Half: white at half alpha over the checkerboard, so between the two.
    let half = h.composite_pixel(&[masked], 30, 32);
    assert!(
        half[0] > 220 && half[0] < 252,
        "a grey mask must be a partial, got {half:?}"
    );
}

#[test]
fn every_level_a_mask_can_hold_moves_the_picture() {
    // **The guard that was missing, at the far end.** `umber_core::docimport::
    // srgb` counts the states a mask slice can hold; this counts the ones that
    // reach the screen, which is the claim that actually matters and the one no
    // CPU test can make.
    //
    // **Measured on the alpha, and that is not a convenience.** A mask
    // multiplies the layer's alpha, which is linear 8-bit; the *colour* the
    // composite hands the screen has been through the sRGB encode, whose slope
    // at the reveal end is about a sixth, so 56 near-white greys are 27 pixels
    // there whatever the mask holds. Reading the colour would measure the
    // display encode and call it a mask defect. `export_rgba` is the one path
    // that hands back straight alpha, and it reuses this very composite pass.
    //
    // **The *hide* end is what is swept, and the first draft swept the reveal
    // end on a figure that was made up.** It said the old storage "could express
    // 22 of the 56" over 200..=255; measured, it expresses all 56 there, because
    // `srgb_to_linear` is *steep* at the top — high stored bytes spread out and
    // collide nowhere. Its toe is where alpha states collapse: over stored
    // 0..=55 linear reaches 56 distinct alphas and sRGB reaches **11**. So the
    // count below only means something down here, and up there it would have
    // been a tautology dressed as a measurement — which is this file's own rule
    // about a figure in a comment being what the next change gets argued
    // against, with the figure invented.
    //
    // See `umber_core::docimport::srgb` for the half of this that goes the other
    // way: at the hide end over an *opaque* backdrop the colour granularity gets
    // worse, and the change is a trade rather than a free win.
    let mut h = harness_or_skip!();
    h.canvas.ensure_slots(&h.gpu.device, &h.gpu.queue, 2);
    fill_slot(&mut h, 0, [255, 255, 255, 255]);
    fill_slot(&mut h, 1, [255, 255, 255, 255]);
    h.canvas.mark_mask_slot(1);

    let mut masked = layer(0, 1.0, BlendMode::Normal);
    masked.mask = Some(1);

    let mut seen = std::collections::BTreeSet::new();
    for level in 0..=55u8 {
        h.canvas.write_layer_rect(
            &h.gpu.device,
            &h.gpu.queue,
            1,
            PixelRect {
                x: 0,
                y: 0,
                width: DOC,
                height: DOC,
            },
            &[level, level, level, 255].repeat((DOC * DOC) as usize),
        );
        let px = h.canvas.export_rgba(&h.gpu.device, &h.gpu.queue, &[masked]);
        let at = ((32 * DOC + 32) * 4) as usize;
        // The layer is opaque, so the alpha *is* the multiplier back again.
        assert_eq!(
            px[at + 3],
            level,
            "mask level {level} did not reach the alpha"
        );
        seen.insert(px[at + 3]);
    }
    // Every one of the 56 lands somewhere of its own, against 11 for the storage
    // this replaced. A regression here is a *shortfall* rather than a wrong
    // pixel, which is exactly why counting is what catches it and why the spot
    // check this file already had could not.
    assert_eq!(
        seen.len(),
        56,
        "levels 0..=55 collapsed into {} distinct alphas; the sRGB storage this \
         replaced reaches 11 here",
        seen.len()
    );
}

#[test]
fn no_mask_is_the_exact_identity() {
    // The rule every optional factor in this engine holds to, and the reason a
    // mask is an `Option<u32>` rather than a slice reserved per layer: a
    // document that has never used one must composite to the same bytes it did
    // before masks existed.
    let mut h = harness_or_skip!();
    h.canvas.ensure_slots(&h.gpu.device, &h.gpu.queue, 2);
    fill_slot(&mut h, 0, [90, 140, 210, 255]);
    // A slice that would hide everything if it were ever read.
    fill_slot(&mut h, 1, [0, 0, 0, 255]);

    let plain = layer(0, 1.0, BlendMode::Normal);
    assert!(plain.mask.is_none(), "the fixture is not testing anything");
    assert_eq!(
        h.composite_pixel(&[plain], 32, 32),
        h.composite_pixel(&[plain], 32, 32),
    );
    assert_near(
        h.composite_pixel(&[plain], 32, 32),
        [90, 140, 210],
        2,
        "an unmasked layer must composite untouched",
    );
}

#[test]
fn a_clipped_layer_is_bounded_by_the_one_below() {
    // Clipping is the layer below's *alpha* and nothing else. The base covers
    // the left half only, so the clipped layer must vanish on the right — and
    // must be untouched on the left, because the base is opaque there.
    let mut h = harness_or_skip!();
    h.canvas.ensure_slots(&h.gpu.device, &h.gpu.queue, 2);

    // Base: opaque red on the left half, nothing on the right.
    h.write_block(
        0,
        PixelRect {
            x: 0,
            y: 0,
            width: DOC / 2,
            height: DOC,
        },
        [200, 40, 40, 255],
    );
    fill_slot(&mut h, 1, [255, 255, 255, 255]);

    let base = layer(0, 1.0, BlendMode::Normal);
    let mut clipped = layer(1, 1.0, BlendMode::Normal);
    clipped.clipped = true;

    assert_near(
        h.composite_pixel(&[base, clipped], 10, 32),
        [255, 255, 255],
        2,
        "where the base is opaque the clipped layer is untouched",
    );
    let outside = h.composite_pixel(&[base, clipped], 54, 32);
    assert!(
        outside[0] > 190 && outside[0] < 235,
        "the clipped layer painted outside its base: {outside:?}"
    );

    // And unclipped it covers everything, which is what says the test above
    // measured the flag rather than the fixture.
    let free = layer(1, 1.0, BlendMode::Normal);
    assert_near(
        h.composite_pixel(&[base, free], 54, 32),
        [255, 255, 255],
        2,
        "without the flag the layer covers the canvas",
    );
}

#[test]
fn a_run_of_clipped_layers_answers_to_the_nearest_unclipped_one() {
    // Two clipped layers in a row are both bound by the base *below the run*,
    // not each by the one immediately beneath. Reading it the other way would
    // make the second follow the first, which is what a naive "previous
    // layer's alpha" would do and is not what any application means.
    let mut h = harness_or_skip!();
    h.canvas.ensure_slots(&h.gpu.device, &h.gpu.queue, 3);

    // Base opaque on the left quarter only.
    h.write_block(
        0,
        PixelRect {
            x: 0,
            y: 0,
            width: DOC / 4,
            height: DOC,
        },
        [200, 40, 40, 255],
    );
    // The first clipped layer covers only the left *half*, so if the second
    // followed it rather than the base, the second would show between the
    // quarter and the half.
    h.write_block(
        1,
        PixelRect {
            x: 0,
            y: 0,
            width: DOC / 2,
            height: DOC,
        },
        [40, 200, 40, 255],
    );
    fill_slot(&mut h, 2, [40, 40, 220, 255]);

    let base = layer(0, 1.0, BlendMode::Normal);
    let mut first = layer(1, 1.0, BlendMode::Normal);
    first.clipped = true;
    let mut second = layer(2, 1.0, BlendMode::Normal);
    second.clipped = true;

    let stack = [base, first, second];
    // Inside the base: the topmost clipped layer wins.
    assert_near(
        h.composite_pixel(&stack, 5, 32),
        [40, 40, 220],
        3,
        "inside the base",
    );
    // Beyond the base but inside the first clipped layer's own pixels: nothing,
    // because both are bound by the base.
    let beyond = h.composite_pixel(&stack, 24, 32);
    assert!(
        beyond[0] > 190 && beyond[0] < 235,
        "a clipped layer followed the clipped layer below it: {beyond:?}"
    );
}

#[test]
fn a_clipped_layer_at_the_bottom_of_the_stack_shows_nothing() {
    // There is no unclipped layer beneath it to be bounded by, so it is bounded
    // by nothing. The alternative — treating "no base" as fully opaque — would
    // make the flag mean something different at the bottom of the stack than it
    // means anywhere else.
    let mut h = harness_or_skip!();
    fill_slot(&mut h, 0, [255, 255, 255, 255]);
    let mut clipped = layer(0, 1.0, BlendMode::Normal);
    clipped.clipped = true;

    let px = h.composite_pixel(&[clipped], 32, 32);
    assert!(
        px[0] > 190 && px[0] < 235,
        "expected the checkerboard, got {px:?}"
    );
}

#[test]
fn a_stroke_on_a_mask_previews_exactly_as_it_commits() {
    // The invariant the whole edit-target design exists to hold. The preview
    // blends the scratch into the mask inside `composite.wgsl`; the commit
    // bakes the same scratch into the mask *slice* through `commit.wgsl`. Two
    // implementations of one blend, exactly as the stroke path already has —
    // and the failure, if they part company, is the mask jumping at pointer-up.
    let mut h = harness_or_skip!();
    h.canvas.ensure_slots(&h.gpu.device, &h.gpu.queue, 2);

    fill_slot(&mut h, 0, [255, 255, 255, 255]);
    fill_slot(&mut h, 1, [255, 255, 255, 255]);
    // **Slot 1 is a mask and has to say so.** The class is the store's own
    // record of it and is already load-bearing without this: `back_tiles` clears
    // a fresh cell to the class's empty value, so a mask that had not declared
    // itself would clear *transparent* and hide the layer wherever the stroke
    // reached a tile nobody had painted. It now decides which view of the page
    // the commit renders through as well, because a mask slice holds linear
    // coverage where a layer's holds sRGB colour — so leaving it out here made
    // the commit write the old form under a composite reading the new one, and
    // the mask jumped at pointer-up by eight levels.
    h.canvas.mark_mask_slot(1);

    let mut masked = layer(0, 1.0, BlendMode::Normal);
    masked.mask = Some(1);

    // Black paint on the mask hides. A partial coverage rather than a solid
    // one, so both the blend and the opacity are actually exercised.
    let style = StrokeStyle {
        color: Color::BLACK,
        opacity: 0.75,
        mode: BrushMode::Paint,
        blend: BlendMode::Normal,
        per_dab_color: false,
        on_mask: true,
    };
    // Before anything is painted, so the assertion below is about the stroke
    // rather than about a comparison two identities would also satisfy.
    let untouched = h.composite_pixel(&[masked], 32, 32);

    h.stamp(&[dab(32.0, 32.0, 14.0, 1.0)]);
    let previewed = h.composite_pixel_with(&[masked], style, 32, 32);
    assert!(
        untouched[0].abs_diff(previewed[0]) > 8,
        "the preview did not hide anything: {untouched:?} then {previewed:?}"
    );

    let size = h.canvas.doc_size();
    let rect = PixelRect {
        x: 0,
        y: 0,
        width: size.x,
        height: size.y,
    };
    let mut enc = h.encoder();
    // Into the mask's slice, which is the whole of what "painting the mask"
    // means to the renderer — `commit_stroke` has no variant for it.
    h.canvas.commit_stroke(
        &h.gpu.device,
        &h.gpu.queue,
        &mut enc,
        1,
        rect,
        &[rect],
        style,
    );
    h.gpu.queue.submit(Some(enc.finish()));

    let committed = h.composite_pixel(&[masked], 32, 32);
    for c in 0..3 {
        assert!(
            previewed[c].abs_diff(committed[c]) <= 2,
            "the mask jumped at pointer-up: previewed {previewed:?}, committed {committed:?}"
        );
    }

    // **And the byte in the slice is the multiplier itself.** The agreement
    // above is a comparison of two composites, and it would hold just as well if
    // both sides used the old sRGB form — which is exactly what it did before
    // the class was declared, in the direction where they happened to agree. So
    // read the slice.
    //
    // The stroke is black at 0.75 opacity over full coverage on a white mask, so
    // the multiplier is 0.25 and the byte is 64. Under the sRGB form the same
    // multiplier is stored as 137, which is nowhere near the slack here — the
    // two readings are 73 levels apart at this coverage, which is why a partial
    // is the case to drive and 0 or 255 would see nothing at all.
    let slice = h.canvas.read_layer_rect(
        &h.gpu.device,
        &h.gpu.queue,
        1,
        PixelRect {
            x: 32,
            y: 32,
            width: 1,
            height: 1,
        },
    );
    assert!(
        slice[0].abs_diff(64) <= 2,
        "a mask slice holds the linear multiplier; got {} where 64 is 0.25 and \
         137 would be the sRGB form this replaced",
        slice[0]
    );
}

#[test]
fn a_masked_layer_clips_what_is_clipped_to_it() {
    // How the two features compose, which is the question a stack of both
    // raises: the base's alpha is what it is *after* its own mask, so hiding
    // part of a base hides what is clipped to it. Anything else would let a
    // clipped layer paint through a hole its base does not fill.
    let mut h = harness_or_skip!();
    h.canvas.ensure_slots(&h.gpu.device, &h.gpu.queue, 3);

    fill_slot(&mut h, 0, [200, 40, 40, 255]);
    // The base's mask: white on the left, black on the right.
    fill_slot(&mut h, 1, [255, 255, 255, 255]);
    h.canvas.write_layer_rect(
        &h.gpu.device,
        &h.gpu.queue,
        1,
        PixelRect {
            x: DOC / 2,
            y: 0,
            width: DOC / 2,
            height: DOC,
        },
        &[0u8, 0, 0, 255].repeat(((DOC / 2) * DOC) as usize),
    );
    fill_slot(&mut h, 2, [40, 40, 220, 255]);

    let mut base = layer(0, 1.0, BlendMode::Normal);
    base.mask = Some(1);
    let mut clipped = layer(2, 1.0, BlendMode::Normal);
    clipped.clipped = true;

    assert_near(
        h.composite_pixel(&[base, clipped], 10, 32),
        [40, 40, 220],
        3,
        "where the base's mask reveals it",
    );
    let hidden = h.composite_pixel(&[base, clipped], 54, 32);
    assert!(
        hidden[0] > 190 && hidden[0] < 235,
        "the clipped layer showed where its base was masked away: {hidden:?}"
    );
}

// ---------------------------------------------------------------------------
// Document background
// ---------------------------------------------------------------------------

#[test]
fn a_transparent_background_leaves_the_checkerboard_alone() {
    // The identity case, and the one that must stay exactly what it was: with
    // an all-zero background the composite's `acc + bg * (1 - acc.a)` adds
    // nothing, so an unpainted canvas is still the transparency checkerboard.
    let mut h = harness_or_skip!();
    h.set_background(Background::Transparent);

    let px = h.composite_pixel(&[layer(0, 1.0, BlendMode::Normal)], 2, 2);
    // The lighter of the checker's two greys, sRGB 0.88.
    assert_near(px, [224, 224, 224], 3, "unpainted, no background");
}

#[test]
fn a_document_background_shows_where_the_stack_does_not() {
    let mut h = harness_or_skip!();
    h.set_background(Background::WHITE);

    let px = h.composite_pixel(&[layer(0, 1.0, BlendMode::Normal)], 2, 2);
    assert_near(px, [255, 255, 255], 2, "white background, nothing painted");
    assert_eq!(px[3], 255, "the screen is always opaque");

    h.set_background(Background::BLACK);
    let px = h.composite_pixel(&[layer(0, 1.0, BlendMode::Normal)], 2, 2);
    assert_near(px, [0, 0, 0], 2, "black background, nothing painted");
}

#[test]
fn the_background_is_under_the_stack_and_not_over_it() {
    // The test that tells the two apart. A half-opaque white layer over a black
    // background is 0.5 in *linear* light, which displays as sRGB ~188 — the
    // same identity `layer_opacity_blends_toward_what_is_beneath` uses. Drawn
    // over the stack instead, the background would simply be black.
    let mut h = harness_or_skip!();
    h.fill(0, Color::WHITE);
    h.set_background(Background::BLACK);

    let px = h.composite_pixel(&[layer(0, 0.5, BlendMode::Normal)], 32, 32);
    assert_near(px, [188, 188, 188], 4, "50% white over a black background");
}

#[test]
fn an_opaque_layer_hides_the_background_entirely() {
    // Source-over with an opaque source is the exact identity, whatever is
    // beneath — so a painted pixel must not shift by a level when the document
    // gains a background.
    let mut h = harness_or_skip!();
    let ink = Color::from_srgb_u8(120, 60, 30, 255);
    h.fill(0, ink);

    let stack = [layer(0, 1.0, BlendMode::Normal)];
    let bare = h.composite_pixel(&stack, 32, 32);
    h.set_background(Background::WHITE);
    assert_eq!(
        h.composite_pixel(&stack, 32, 32),
        bare,
        "an opaque pixel must not move when a background is added under it"
    );
}

#[test]
fn export_carries_the_background_without_a_path_of_its_own() {
    // `export_rgba` reuses the screen composite with an export flag, and the
    // background is applied before that branch — so both halves fall out of one
    // line: a transparent document still exports with its alpha, and a
    // white-backed one exports opaque white.
    let mut h = harness_or_skip!();
    h.stamp(&[dab(32.0, 32.0, 6.0, 1.0)]);
    h.commit(Color::BLACK, 1.0, BrushMode::Paint);

    let stack = [layer(0, 1.0, BlendMode::Normal)];
    let corner = ((2 * DOC + 2) * 4) as usize;
    let centre = ((32 * DOC + 32) * 4) as usize;
    let at = |px: &[u8], i: usize| [px[i], px[i + 1], px[i + 2], px[i + 3]];

    h.set_background(Background::Transparent);
    let clear = h.canvas.export_rgba(&h.gpu.device, &h.gpu.queue, &stack);
    assert_eq!(
        at(&clear, corner),
        [0, 0, 0, 0],
        "a transparent document must still export with its alpha"
    );

    h.set_background(Background::WHITE);
    let white = h.canvas.export_rgba(&h.gpu.device, &h.gpu.queue, &stack);
    assert_eq!(
        at(&white, corner),
        [255, 255, 255, 255],
        "a white-backed document must export opaque white"
    );
    // Straight alpha, so the ink is unchanged by what is behind it.
    assert_eq!(
        at(&white, centre),
        at(&clear, centre),
        "the painted pixel must not move when the background does"
    );
}

// ---------------------------------------------------------------------------
// Resizing the canvas
// ---------------------------------------------------------------------------

#[test]
fn resizing_the_canvas_carries_the_artwork_to_its_anchor() {
    let mut h = harness_or_skip!();
    h.stamp(&[dab(16.0, 16.0, 5.0, 1.0)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);
    assert_eq!(
        h.pixel(16, 16)[3],
        255,
        "the mark should be where it landed"
    );

    // 64 -> 128 held at the centre offsets everything by 32.
    h.canvas.resize(
        &h.gpu.device,
        &h.gpu.queue,
        UVec2::splat(128),
        Anchor::Centre,
        1,
    );
    assert_eq!(h.canvas.doc_size(), UVec2::splat(128));

    assert_eq!(
        h.pixel_in(0, 48, 48)[3],
        255,
        "the mark must move with the canvas it was painted on"
    );
    assert_eq!(
        h.pixel_in(0, 16, 16)[3],
        0,
        "where it used to be is now new canvas"
    );
    assert_eq!(
        h.pixel_in(0, 120, 120)[3],
        0,
        "new canvas must start clear rather than holding whatever was allocated"
    );
}

#[test]
fn cropping_a_canvas_keeps_the_anchored_corner() {
    let mut h = harness_or_skip!();
    h.stamp(&[dab(8.0, 8.0, 4.0, 1.0), dab(56.0, 56.0, 4.0, 1.0)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);

    // Held at the top left, the far mark is the one that falls off the edge.
    h.canvas.resize(
        &h.gpu.device,
        &h.gpu.queue,
        UVec2::splat(32),
        Anchor::TopLeft,
        1,
    );
    assert_eq!(h.canvas.doc_size(), UVec2::splat(32));
    assert_eq!(h.pixel_in(0, 8, 8)[3], 255, "the near mark should survive");
}

#[test]
fn every_layer_moves_together_when_the_canvas_does() {
    // The anchor moves the *picture*, so the copy is one transfer across every
    // slice. A per-slice loop that got an origin wrong would show up as one
    // layer sliding relative to another, which is the worst kind of quiet.
    let mut h = harness_or_skip!();
    h.fill(0, Color::from_srgb_u8(200, 40, 40, 255));
    h.stamp(&[dab(20.0, 20.0, 5.0, 1.0)]);
    h.commit_to(1, Color::WHITE, 1.0, BrushMode::Paint);

    h.canvas.resize(
        &h.gpu.device,
        &h.gpu.queue,
        UVec2::new(96, 96),
        Anchor::BottomRight,
        2,
    );

    // Held at the bottom right, growing by 32 on each axis offsets by 32.
    assert_near(
        h.pixel_in(0, 52, 52),
        [200, 40, 40],
        2,
        "slot 0 after resize",
    );
    assert_eq!(h.pixel_in(1, 52, 52)[3], 255, "slot 1 moved with slot 0");
}

#[test]
fn a_stroke_painted_after_a_resize_lands_where_it_is_aimed() {
    // The dab pass turns document pixels into clip space with the canvas size
    // out of its own uniform block. A resize that reallocated the textures but
    // left that number behind would put every later dab at the wrong place and
    // the wrong scale — and the layer would still look plausible.
    let mut h = harness_or_skip!();
    h.canvas.resize(
        &h.gpu.device,
        &h.gpu.queue,
        UVec2::splat(128),
        Anchor::TopLeft,
        1,
    );

    h.stamp(&[dab(100.0, 100.0, 5.0, 1.0)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);

    assert_eq!(h.pixel_in(0, 100, 100)[3], 255, "the dab missed its mark");
    assert_eq!(
        h.pixel_in(0, 60, 60)[3],
        0,
        "and it should not be elsewhere"
    );
}

/// A renderer is built at the slot count its document needs, so nothing has to
/// grow it afterwards.
///
/// Two properties, and the second is what stops the first being over-corrected.
///
/// **Built at the count.** A growth holds the old array and the new one at once,
/// so an import arriving a slice at a time paid that peak at every step. The
/// output measured is the capacity that was allocated; asking `ensure_slots` for
/// the same count afterwards must move nothing, which is what lets
/// `Graphics::add_canvas` drop its growth — and with the growth goes the second
/// clear, since `ensure_slots` clears every slice it adds and `clear_all_layers`
/// then cleared them again.
///
/// **The speculative floor survives.** `initial_slots` is still the minimum, so
/// a blank one-layer document does not reallocate the moment a second layer
/// arrives.
#[test]
fn a_renderer_is_built_at_its_documents_slot_count_and_keeps_the_speculation() {
    let h = harness_or_skip!();

    let mut deep = h
        .canvas
        .for_document(&h.gpu.device, &h.gpu.queue, UVec2::splat(64), 21);
    assert!(
        deep.slot_capacity() >= 21,
        "a twenty-one layer document was built at {} slices",
        deep.slot_capacity()
    );
    let built = deep.slot_capacity();
    deep.ensure_slots(&h.gpu.device, &h.gpu.queue, 21);
    assert_eq!(
        deep.slot_capacity(),
        built,
        "the document's own count still had to be grown into"
    );

    let shallow = h
        .canvas
        .for_document(&h.gpu.device, &h.gpu.queue, UVec2::splat(64), 1);
    assert!(
        shallow.slot_capacity() > 1,
        "an ordinary document lost the handful of slices it speculates on"
    );
}

/// A reservation the device can satisfy must build **exactly** the renderer the
/// infallible path would have, and nothing about the picture may move.
///
/// This is the constraint the whole refusal path is held to: it changes what
/// happens when an allocation fails and must change nothing about what happens
/// when one succeeds. The two easy ways to break that are a different capacity —
/// `try_with_shared` computing its own rather than sharing `built_capacity` —
/// and a renderer assembled differently, which would show up as pixels.
///
/// So it measures both: the capacity that was allocated, against the infallible
/// twin asked the same question, and a stroke committed through the reserved
/// renderer read back off its own slice.
///
/// **It does not exercise a refusal.** See
/// `a_reservation_builds_no_view_before_it_has_checked` in `canvas.rs` for why
/// nothing here provokes a real out-of-memory.
#[test]
fn a_reservation_that_fits_builds_what_the_infallible_path_builds() {
    let mut h = harness_or_skip!();

    let expected = h
        .canvas
        .for_document(&h.gpu.device, &h.gpu.queue, UVec2::splat(64), 21);
    let reserved = h
        .canvas
        .try_for_document(&h.gpu.device, &h.gpu.queue, UVec2::splat(64), 21)
        .expect("64 square by 21 slices is 344 KB; no device refuses that");
    assert_eq!(
        reserved.slot_capacity(),
        expected.slot_capacity(),
        "the fallible door allocated a different array from the one beside it"
    );

    drop(reserved);

    // And the reserved renderer is a renderer: paint through it and read the
    // slice back. A store built from a texture the caller passed in is where an
    // array view left unbuilt, or built against the wrong texture, would show.
    h.canvas = h
        .canvas
        .try_for_document(&h.gpu.device, &h.gpu.queue, UVec2::new(DOC, DOC), 4)
        .expect("the harness canvas, reserved rather than assumed");
    let mut enc = h.encoder();
    h.canvas.clear_all_layers(&h.gpu.queue);
    h.canvas.clear_stroke(&h.gpu.device, &mut enc);
    h.gpu.queue.submit(Some(enc.finish()));

    h.stamp(&[dab(32.0, 32.0, 10.0, 1.0)]);
    h.commit_to(
        0,
        Color::from_srgb_u8(200, 40, 40, 255),
        1.0,
        BrushMode::Paint,
    );
    assert_near(
        h.pixel_in(0, 32, 32),
        [200, 40, 40],
        2,
        "a stroke committed into a reserved array",
    );
}

/// The fallible growth is the same growth: it copies the picture across and
/// clears what it added.
///
/// The twin of `growing_the_layer_array_preserves_existing_pixels`, and it
/// exists because `ensure_slots` was split into a decision and a copy so the two
/// doors could share both. A copy the fallible door forgot would lose the
/// artist's work on the frame they added a layer, and the guard on the
/// infallible door cannot see it.
#[test]
fn growing_through_the_fallible_door_preserves_existing_pixels() {
    let mut h = harness_or_skip!();

    h.fill(0, Color::from_srgb_u8(200, 40, 40, 255));
    assert!(h.canvas.page_count() < 8);

    h.canvas
        .try_ensure_pages(&h.gpu.device, &h.gpu.queue, None, 8)
        .expect("eight pages of a 64-square canvas is 512 KB");
    assert!(h.canvas.page_count() >= 8);

    assert_near(h.pixel_in(0, 32, 32), [200, 40, 40], 2, "after growth");
    assert_eq!(
        h.pixel_in(7, 32, 32)[3],
        0,
        "a slot nothing has painted on reads as transparent"
    );

    // Asked again for what it already holds, it must allocate nothing at all —
    // the early return is what makes a reservation free on every add that fits.
    let held = h.canvas.page_count();
    h.canvas
        .try_ensure_pages(&h.gpu.device, &h.gpu.queue, None, 8)
        .expect("nothing to do cannot fail");
    assert_eq!(h.canvas.page_count(), held);

    // And `try_ensure_slots` is a *headroom* reservation rather than a slice: a
    // blank layer costs nothing, so there is no slice left to refuse. It must
    // still be idempotent — sixty-four blank layers grow the atlas once, not
    // sixty-four times, which is the whole difference between a headroom check
    // and a per-layer allocation.
    let held = h.canvas.page_count();
    for slot in 0..64 {
        h.canvas
            .try_ensure_slots(&h.gpu.device, &h.gpu.queue, slot + 1)
            .expect("a blank layer asks for nothing");
    }
    assert_eq!(
        h.canvas.page_count(),
        held,
        "adding blank layers must not grow the atlas"
    );
}

/// A resize rebuilds the array at the *live* slice count, not the one the old
/// canvas happened to be holding.
///
/// The failure this guards is silent and enormous: a 512² document legitimately
/// holding 256 slices is 256 MiB, and the same capacity carried onto a 10000²
/// canvas is 102.4 GB. The document arrives at it through a dialog rather than
/// through the growth rule, so nothing in `grown_capacity` can see it.
///
/// **It measures the capacity that was allocated**, which is the output, rather
/// than restating the rule — and it drives the *shrink*, which is the direction
/// the bug is in. A capacity of 64 was reachable here before this test existed.
///
/// Two live slices at 128² comes out at **four**, not two, and that is
/// `built_capacity`'s speculative floor rather than slack: a resized document
/// gets the same handful of spare slices a freshly opened one of its shape does,
/// or the next layer added after a resize pays a whole reallocate-and-copy that
/// the same layer added before it would not.
#[test]
fn a_resize_rebuilds_at_the_live_slice_count_and_carries_the_picture() {
    let mut h = harness_or_skip!();
    // Deliberately far more than the document holds. `ensure_slots` never
    // shrinks, so this is exactly the state a delete-then-add session leaves.
    h.canvas.ensure_pages(&h.gpu.device, &h.gpu.queue, None, 64);
    assert_eq!(h.canvas.page_count(), 64, "the atlas should have grown");

    h.fill(0, Color::from_srgb_u8(200, 40, 40, 255));
    h.stamp(&[dab(20.0, 20.0, 5.0, 1.0)]);
    h.commit_to(1, Color::WHITE, 1.0, BrushMode::Paint);

    h.canvas.resize(
        &h.gpu.device,
        &h.gpu.queue,
        UVec2::splat(128),
        Anchor::TopLeft,
        2,
    );

    assert_eq!(
        h.canvas.page_count(),
        4,
        "the new atlas carried the old canvas's page count"
    );
    // And the shorter copy still carried every slice that mattered: the depth
    // is `min(old, new)`, so trimming the capacity must not trim the picture.
    assert_near(
        h.pixel_in(0, 20, 20),
        [200, 40, 40],
        2,
        "slot 0 after a shrinking resize",
    );
    assert_eq!(
        h.pixel_in(1, 20, 20)[3],
        255,
        "slot 1 after a shrinking resize"
    );
}

#[test]
fn the_eyedropper_can_pick_the_background_up() {
    // `pick_colour` reuses the same pass, so this is the same line of shader
    // rather than a second implementation — but it is worth pinning, because
    // picking on blank canvas used to mean "nothing there" and now legitimately
    // means "the paper".
    let mut h = harness_or_skip!();
    h.set_background(Background::Colour(Color::from_srgb_u8(200, 40, 40, 255)));

    let px = h.canvas.pick_colour(
        &h.gpu.device,
        &h.gpu.queue,
        &[layer(0, 1.0, BlendMode::Normal)],
        Vec2::new(8.0, 8.0),
    );
    assert_eq!(
        px[3], 255,
        "the background is opaque, so there is something"
    );
    assert_near(px, [200, 40, 40], 3, "picked background");
}

/// Four opaque pixels of known colour meeting at the document corner
/// `(21, 21)`, on an otherwise empty layer.
///
/// A corner rather than a middle deliberately: it is the position at which a
/// bilinear tap of the layer array is the flat average of all four, so it tells
/// "the pixel the point is in" from "whatever the sampler resolved" by the
/// widest possible margin. Every other fixture in this file picks at
/// `(32.5, 32.5)`, which is a pixel centre and therefore the one coordinate
/// where the two readings agree.
fn four_pixels_at_a_corner(h: &mut Harness) {
    for (x, y, rgba) in [
        (20u32, 20u32, [255u8, 0, 0, 255]),
        (21, 20, [0, 255, 0, 255]),
        (20, 21, [0, 0, 255, 255]),
        (21, 21, [255, 255, 0, 255]),
    ] {
        h.write_block(
            0,
            PixelRect {
                x,
                y,
                width: 1,
                height: 1,
            },
            rgba,
        );
    }
}

#[test]
fn a_pick_takes_the_pixel_it_is_over_rather_than_a_blend_of_four() {
    // `composite.wgsl` samples the layer array through a `Linear` filter at
    // `uv = doc / doc_size`, so a texel centre is only hit where the document
    // coordinate is `n + 0.5` — and `screen_to_doc` is a camera transform,
    // which hands over an arbitrary fraction. Unsnapped, an eyedropper
    // therefore took a bilinear blend of up to four pixels, and at a pixel
    // *corner* it took the flat average of all four. `pick_patch` snaps the
    // camera to `floor + 0.5` for exactly this.
    //
    // This was live for as long as the eyedropper has existed and no test could
    // see it, because the two that pick both aim at `(32.5, 32.5)`.
    let mut h = harness_or_skip!();
    four_pixels_at_a_corner(&mut h);
    let stack = [layer(0, 1.0, BlendMode::Normal)];

    for (at, want, what) in [
        (
            Vec2::new(21.0, 21.0),
            [255u8, 255, 0],
            "the corner all four meet at",
        ),
        (
            Vec2::new(20.0, 20.0),
            [255, 0, 0],
            "the far corner of the block",
        ),
        (
            Vec2::new(21.4, 20.6),
            [0, 255, 0],
            "inside the top-right pixel",
        ),
        (
            Vec2::new(20.6, 21.4),
            [0, 0, 255],
            "inside the bottom-left pixel",
        ),
        (
            Vec2::new(21.5, 21.5),
            [255, 255, 0],
            "and a pixel centre, unchanged",
        ),
    ] {
        let px = h
            .canvas
            .pick_colour(&h.gpu.device, &h.gpu.queue, &stack, at);
        assert_eq!(px[3], 255, "{what}: the pixel is opaque");
        assert_near(px, want, 3, what);
    }
}

#[test]
fn a_block_of_the_picture_holds_the_pixels_around_the_one_a_click_takes() {
    // The loupe's neighbourhood, and the only test of `pick_patch` at a size
    // greater than one — which is the size where the pivot, the snap and the
    // caller's `first = floor(doc) - size / 2` all have to agree. At a size of
    // one every one of those offsets is zero, so `pick_colour` exercises none
    // of them: reverting the pivot to the old `(0.5, 0.5)` leaves the whole
    // workspace green while moving the loupe five pixels off the pointer and
    // the colour taken with it.
    let mut h = harness_or_skip!();
    four_pixels_at_a_corner(&mut h);
    let stack = [layer(0, 1.0, BlendMode::Normal)];

    // Deliberately not a pixel centre and not a corner: an ordinary place for a
    // pointer to be. `floor` is (21, 21), so the middle texel is that pixel and
    // texel `k` is `21 + (k - 1)`.
    let at = Vec2::new(21.3, 21.7);
    let block = h
        .canvas
        .pick_patch(&h.gpu.device, &h.gpu.queue, &stack, at, 3);
    assert_eq!(block.len(), 3 * 3 * 4, "row-major, tightly packed");
    let texel = |col: usize, row: usize| {
        let i = (row * 3 + col) * 4;
        [block[i], block[i + 1], block[i + 2], block[i + 3]]
    };

    assert_near(texel(0, 0), [255, 0, 0], 3, "texel (0,0) is pixel (20,20)");
    assert_near(texel(1, 0), [0, 255, 0], 3, "texel (1,0) is pixel (21,20)");
    assert_near(texel(0, 1), [0, 0, 255], 3, "texel (0,1) is pixel (20,21)");
    assert_near(
        texel(1, 1),
        [255, 255, 0],
        3,
        "texel (1,1) is pixel (21,21)",
    );
    // The right column and the bottom row fall on empty layer, which is what
    // says the block is centred rather than anchored at its top-left.
    for (col, row) in [(2, 0), (2, 1), (2, 2), (0, 2), (1, 2)] {
        assert_eq!(
            texel(col, row)[3],
            0,
            "texel ({col},{row}) is off the painted block"
        );
    }

    // And the middle texel is what a click would keep, which is the promise
    // `pick_colour` being `pick_patch(.., 1)` is supposed to make structural.
    let single = h
        .canvas
        .pick_colour(&h.gpu.device, &h.gpu.queue, &stack, at);
    assert_eq!(
        texel(1, 1),
        single,
        "the middle of the block is the colour the eyedropper takes"
    );
}

/// Drive the smudge probe the way `app.rs` does — record, submit, collect —
/// and give the GPU as long as it needs, since the whole point of the thing is
/// that it does not block.
fn probe_until_ready(h: &mut Harness, stack: &[LayerDraw], at: Vec2, radius: f32) -> [f32; 4] {
    for _ in 0..64 {
        let mut enc = h.encoder();
        h.canvas.probe_canvas(
            &h.gpu.device,
            &h.gpu.queue,
            &mut enc,
            &ProbeParams {
                layers: stack,
                active_index: 0,
                stroke: StrokeStyle {
                    opacity: 0.0,
                    ..Default::default()
                },
                doc_point: at,
                radius,
            },
        );
        h.gpu.queue.submit(Some(enc.finish()));
        h.canvas.submit_probes();
        // Block until the copy submitted above has actually run.
        //
        // `take_probe` polls *without* blocking, which is exactly right on the
        // drawing path — the whole design of the probe is that no frame waits
        // on the GPU — and useless in a loop that has nothing else to do. On a
        // discrete adapter the sample happens to be ready by the next
        // iteration; on a software one, which is what a CI runner without a
        // GPU has, sixty-four non-blocking polls go by in well under a
        // millisecond and the map is never serviced. That is a slow adapter,
        // not a broken probe: the application would simply keep the previous
        // sample for another frame or two, which the smudge is built to
        // tolerate.
        let _ = h.gpu.device.poll(wgpu::PollType::wait_indefinitely());
        if let Some(sample) = h.canvas.take_probe(&h.gpu.device) {
            return sample;
        }
    }
    panic!("the probe never came home");
}

#[test]
fn the_smudge_probe_reads_what_is_under_the_brush() {
    // The asynchronous readback, end to end. A blender is only as good as this:
    // if it never resolves the brush paints its palette colour forever, and if
    // the decode is wrong every smudge is the wrong colour.
    let mut h = harness_or_skip!();
    h.fill(0, Color::from_srgb_u8(200, 20, 20, 255));

    let stack = [layer(0, 1.0, BlendMode::Normal)];
    let sample = probe_until_ready(&mut h, &stack, Vec2::new(32.0, 32.0), 6.0);

    assert!(sample[3] > 0.9, "should have found solid paint: {sample:?}");
    // Linear, not sRGB: sRGB 200 is linear ~0.578. Asserting ~0.78 here would
    // be the classic averaging-gamma-encoded-bytes mistake.
    let expected = Color::from_srgb_u8(200, 20, 20, 255);
    assert!(
        (sample[0] - expected.r).abs() < 0.02 && (sample[1] - expected.g).abs() < 0.02,
        "expected linear {:?}, got {sample:?}",
        [expected.r, expected.g, expected.b]
    );
}

#[test]
fn the_probe_reports_bare_canvas_as_nothing_to_pick_up() {
    // Alpha is what stops a blender dragged off the edge of a painting from
    // smearing black back onto it — `StrokeBuilder::absorb` ignores a sample
    // with no coverage, and can only do that if the probe reports it honestly.
    let mut h = harness_or_skip!();

    let stack = [layer(0, 1.0, BlendMode::Normal)];
    let sample = probe_until_ready(&mut h, &stack, Vec2::new(32.0, 32.0), 6.0);

    assert_eq!(sample[3], 0.0, "empty canvas should report no coverage");
}

/// Ending a stroke must not put a probe buffer back into service while the GPU
/// still owns it.
///
/// `reset_probes` used to mark every slot idle, including ones whose
/// `map_async` had not called back — and a map only completes on a poll, so a
/// stroke ending between frames leaves one behind almost every time. The next
/// stroke was then handed that slot, recorded a copy into it, and `submit`
/// refuses any submission touching a buffer that is mapped or awaiting a map.
/// A validation error aborts the process, so this was "smudge, lift the pen,
/// smudge again" and the app was gone.
#[test]
fn a_probe_still_in_flight_is_not_reused_by_the_next_stroke() {
    let mut h = harness_or_skip!();
    h.fill(0, Color::from_srgb_u8(200, 20, 20, 255));
    let stack = [layer(0, 1.0, BlendMode::Normal)];

    // Fill the rotation and deliberately never poll, so every map is still
    // outstanding — the state a stroke ends in.
    for _ in 0..4 {
        let mut enc = h.encoder();
        record_probe(&mut h, &stack, &mut enc);
        h.gpu.queue.submit(Some(enc.finish()));
        h.canvas.submit_probes();
    }
    h.canvas.reset_probes();

    let scope = h.gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut enc = h.encoder();
    record_probe(&mut h, &stack, &mut enc);
    h.gpu.queue.submit(Some(enc.finish()));
    let error = pollster::block_on(scope.pop());
    assert!(
        error.is_none(),
        "the next stroke reused a buffer the GPU still owns: {error:?}"
    );

    // And the rotation must come back: disowning a slot cannot mean losing it,
    // or the second blender of a session would never sample anything.
    let sample = probe_until_ready(&mut h, &stack, Vec2::new(32.0, 32.0), 6.0);
    assert!(
        sample[3] > 0.9,
        "probes stopped working after a reset: {sample:?}"
    );
}

/// Record one probe into `enc`, with the parameters the other probe tests use.
fn record_probe(h: &mut Harness, stack: &[LayerDraw], enc: &mut wgpu::CommandEncoder) {
    h.canvas.probe_canvas(
        &h.gpu.device,
        &h.gpu.queue,
        enc,
        &ProbeParams {
            layers: stack,
            active_index: 0,
            stroke: StrokeStyle {
                opacity: 0.0,
                ..Default::default()
            },
            doc_point: Vec2::new(32.0, 32.0),
            radius: 6.0,
        },
    );
}

/// A slot the texture array does not have must be refused, not passed to wgpu.
///
/// It should not arise — `ensure_slots` runs before a layer is painted — but a
/// copy naming a missing slice is a validation error, and a validation error
/// takes the process down. The resume path rebuilds storage from scratch with
/// the stack already deep, which is close enough to this to be worth a guard.
#[test]
fn a_layer_copy_beyond_the_array_is_refused_rather_than_fatal() {
    let mut h = harness_or_skip!();
    let beyond = h.canvas.slot_capacity();
    let rect = PixelRect {
        x: 0,
        y: 0,
        width: 4,
        height: 4,
    };

    let scope = h.gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let bytes = h
        .canvas
        .read_layer_rect(&h.gpu.device, &h.gpu.queue, beyond, rect);
    h.canvas
        .write_layer_rect(&h.gpu.device, &h.gpu.queue, beyond, rect, &bytes);
    let error = pollster::block_on(scope.pop());
    assert!(
        error.is_none(),
        "wgpu was handed a missing slice: {error:?}"
    );

    // The size has to be right whatever else happens: `PixelPatch` asserts the
    // byte count matches the rect, and the undo stack is built out of these.
    assert_eq!(bytes.len(), (rect.width * rect.height * 4) as usize);
    assert!(bytes.iter().all(|b| *b == 0), "a missing slice reads blank");
}

/// Every offscreen pass must work when the *window* is a different format.
///
/// This is the shape of a real crash. A render pipeline is compiled against its
/// target's format; export, the eyedropper and the smudge probe all render into
/// `Rgba8Unorm` while the swapchain on a great deal of Windows hardware is
/// `Bgra8Unorm`. Using the screen's pipeline for them is a validation error that
/// takes the process down — and it is completely invisible on a machine whose
/// surface happens to match, which is why every other test here missed it.
#[test]
fn offscreen_passes_work_when_the_surface_is_bgra() {
    let Some(gpu) = shared_gpu() else {
        eprintln!("no adapter; skipping");
        return;
    };
    static SERIAL: Mutex<()> = Mutex::new(());
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    // Deliberately not TARGET_FORMAT: the whole point is a mismatch.
    let mut canvas = CanvasRenderer::new(
        &gpu.device,
        &gpu.queue,
        UVec2::new(DOC, DOC),
        wgpu::TextureFormat::Bgra8Unorm,
        1,
    );

    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    canvas.clear_all_layers(&gpu.queue);
    canvas.clear_stroke(&gpu.device, &mut enc);
    canvas.draw_dabs(
        &gpu.device,
        &gpu.queue,
        &mut enc,
        &[dab(32.0, 32.0, 20.0, 1.0)],
        DabStyle::default(),
    );
    gpu.queue.submit(Some(enc.finish()));

    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    // The whole rect as the single piece: this commits everything it
    // damaged, which is what a piece list of one whole rect means.
    let rect = PixelRect {
        x: 0,
        y: 0,
        width: DOC,
        height: DOC,
    };
    canvas.commit_stroke(
        &gpu.device,
        &gpu.queue,
        &mut enc,
        0,
        rect,
        &[rect],
        StrokeStyle {
            color: Color::from_srgb_u8(200, 20, 20, 255),
            opacity: 1.0,
            mode: BrushMode::Paint,
            blend: BlendMode::Normal,
            per_dab_color: false,
            on_mask: false,
        },
    );
    gpu.queue.submit(Some(enc.finish()));

    let stack = [layer(0, 1.0, BlendMode::Normal)];

    // The eyedropper.
    let px = canvas.pick_colour(&gpu.device, &gpu.queue, &stack, Vec2::new(32.5, 32.5));
    assert_near(px, [200, 20, 20], 3, "picked colour on a BGRA surface");

    // PNG export.
    let bytes = canvas.export_rgba(&gpu.device, &gpu.queue, &stack);
    assert_eq!(bytes.len(), (DOC * DOC * 4) as usize);

    // And the smudge probe, which is what runs every frame of a blending
    // stroke and therefore turns this from latent into fatal.
    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    canvas.probe_canvas(
        &gpu.device,
        &gpu.queue,
        &mut enc,
        &ProbeParams {
            layers: &stack,
            active_index: 0,
            stroke: StrokeStyle {
                opacity: 0.0,
                ..Default::default()
            },
            doc_point: Vec2::new(32.0, 32.0),
            radius: 6.0,
        },
    );
    gpu.queue.submit(Some(enc.finish()));
    canvas.submit_probes();
    let _ = canvas.take_probe(&gpu.device);
}

#[test]
fn picking_reads_the_flattened_stack_not_one_layer() {
    // An eyedropper should return what the user can see, so a layer above must
    // win over the one below it.
    let mut h = harness_or_skip!();
    h.fill(0, Color::from_srgb_u8(20, 200, 20, 255));
    h.fill(1, Color::from_srgb_u8(200, 20, 20, 255));

    let stack = [
        layer(0, 1.0, BlendMode::Normal),
        layer(1, 1.0, BlendMode::Normal),
    ];
    let px = h
        .canvas
        .pick_colour(&h.gpu.device, &h.gpu.queue, &stack, Vec2::new(32.5, 32.5));
    assert_near(px, [200, 20, 20], 3, "picked colour");
}

/// A brush driven by something other than pressure, all the way from the input
/// samples to the committed pixels.
///
/// The engine half is covered by unit tests in `umber-core`; what this adds is
/// that the dabs a modulated stroke produces are still ordinary dab instances,
/// so the wet-layer guarantee holds for them. It would be easy to reach for a
/// second blend state the day per-dab shape started varying.
#[test]
fn a_modulated_stroke_still_saturates_under_overlap() {
    let mut h = harness_or_skip!();

    let brush = Brush {
        size: 24.0,
        spacing: 0.1,
        stabilization: 0.0,
        pressure_size: false,
        pressure_opacity: false,
        dab_ratio: 3.0,
        modulations: [
            Modulation {
                target: DabTarget::Ratio,
                input: DabInput::Random,
                low: -2.0,
                high: 2.0,
                curve: ResponseCurve::LINEAR,
            },
            Modulation {
                target: DabTarget::Size,
                input: DabInput::Speed,
                low: -0.3,
                high: 0.3,
                curve: ResponseCurve::LINEAR,
            },
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    };

    // Scrubbed back and forth over the same spot, which is the case that
    // compounds if the `max` blend is ever lost.
    let mut s = StrokeBuilder::new();
    s.begin(
        brush,
        [1.0, 1.0, 1.0],
        InputPoint::new(Vec2::new(32.0, 32.0), 1.0, 0.0),
    );
    let mut dabs: Vec<Dab> = Vec::new();
    for (i, x) in [20.0, 44.0, 20.0, 44.0].into_iter().enumerate() {
        s.extend(InputPoint::new(
            Vec2::new(x, 32.0),
            1.0,
            (i + 1) as f64 * 0.05,
        ));
        dabs.extend(s.drain_pending());
    }
    assert!(
        dabs.len() > 10,
        "expected a stroke, got {} dabs",
        dabs.len()
    );
    // The dabs really did change shape, or the test proves nothing.
    let lo = dabs.iter().map(|d| d.aspect).fold(f32::MAX, f32::min);
    let hi = dabs.iter().map(|d| d.aspect).fold(f32::MIN, f32::max);
    assert!(hi - lo > 1.0, "aspect did not vary: {lo}..{hi}");
    assert!(lo >= 1.0, "a dab turned inside out at aspect {lo}");

    h.stamp(&dabs);
    h.commit(Color::WHITE, 0.5, BrushMode::Paint);

    let alpha = h.pixel(32, 32)[3];
    assert!(
        (100..=155).contains(&alpha),
        "expected ~128 (one stroke at half opacity), got {alpha} — a modulated \
         stroke is compounding"
    );
}

/// Colour dynamics ride the per-dab colour path smudging already built, so the
/// evidence that they reach the canvas is that two points along one stroke
/// commit at different brightnesses. With the colour scratch ignored the whole
/// mark would come out the flat palette colour.
#[test]
fn a_colour_modulated_stroke_commits_a_different_colour_per_dab() {
    let mut h = harness_or_skip!();

    let brush = Brush {
        size: 8.0,
        spacing: 0.5,
        stabilization: 0.0,
        pressure_size: false,
        modulations: [Modulation {
            target: DabTarget::Value,
            input: DabInput::Random,
            low: -0.45,
            high: 0.45,
            curve: ResponseCurve::LINEAR,
        }]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    assert!(brush.colours_dabs(), "this brush needs the colour scratch");

    let mut s = StrokeBuilder::new();
    let mid = [0.216, 0.216, 0.216]; // linear for sRGB 128
    s.begin(brush, mid, InputPoint::new(Vec2::new(4.0, 32.0), 1.0, 0.0));
    s.extend(InputPoint::new(Vec2::new(60.0, 32.0), 1.0, 0.1));
    let dabs: Vec<Dab> = s.drain_pending().collect();
    assert!(dabs.len() > 10);

    h.stamp_colored(&dabs, true);
    // Black as the stroke colour: anything but black proves the colour scratch
    // was read, and a *range* of values proves it was read per dab.
    h.commit(Color::BLACK, 1.0, BrushMode::Paint);

    let mut lo = u8::MAX;
    let mut hi = u8::MIN;
    for x in (6..58).step_by(2) {
        let px = h.pixel(x, 32);
        if px[3] > 200 {
            lo = lo.min(px[0]);
            hi = hi.max(px[0]);
        }
    }
    assert!(hi > lo, "the whole stroke committed one colour ({lo})");
    assert!(
        hi - lo > 40,
        "brightness barely varied along the stroke: {lo}..{hi}"
    );
}

// ---------------------------------------------------------------------------
// The whole-document capture — the autosave's readback
// ---------------------------------------------------------------------------

/// Drive a capture the way `app.rs` drives it: one step per frame, recorded
/// into the frame's own encoder, mapped after that frame is submitted and
/// collected by a poll that never waits.
///
/// Returns the document and the **longest** single frame's main-thread cost,
/// which is the number that decides whether an autosave is felt.
fn run_capture(
    gpu: &Gpu,
    canvas: &mut CanvasRenderer,
    slots: &[u32],
    draws: &[LayerDraw],
) -> (DocumentCapture, std::time::Duration, usize) {
    assert!(
        canvas.begin_capture(slots, draws),
        "a capture was in flight"
    );
    drive_to_completion(gpu, canvas)
}

/// The frame loop of [`run_capture`], for a capture already begun.
fn drive_to_completion(
    gpu: &Gpu,
    canvas: &mut CanvasRenderer,
) -> (DocumentCapture, std::time::Duration, usize) {
    let mut worst = std::time::Duration::ZERO;
    let mut frames = 0usize;
    // **This loop is a wall-clock budget in disguise, and saying it was not is
    // what made it flaky.** The comment here used to read "a capture that has
    // not finished inside this is a bug, not a slow machine" — but the sleep at
    // the foot of the body is what paces it, so 2000 iterations was about two
    // seconds of real time, and a machine building six other things does not
    // finish a banded capture of a large document in two. Measured: twelve runs
    // green on an idle machine and six failures in ten under load, on code that
    // had not changed. That is the shape CLAUDE.md's "nothing here may assert
    // wall-clock time on CI" warns about, arriving through a *frame count*
    // rather than through a `Duration`, which is why it was not recognised.
    //
    // So the bound is deliberately far past anything a real capture needs, and
    // the frame count is kept only to catch a genuine hang — a capture that has
    // stopped making progress will never come home however long it is given.
    const FRAME_BUDGET: usize = 40_000;
    for _ in 0..FRAME_BUDGET {
        frames += 1;
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

        // Everything the autosave adds to a frame. The `queue.submit` between
        // the two halves is the frame's own and is excluded on purpose.
        let started = std::time::Instant::now();
        canvas.drive_capture(&gpu.device, &gpu.queue, &mut enc);
        let recording = started.elapsed();

        gpu.queue.submit(Some(enc.finish()));

        let started = std::time::Instant::now();
        canvas.submit_capture();
        let taken = canvas.take_capture(&gpu.device);
        worst = worst.max(recording + started.elapsed());

        if let Some(doc) = taken {
            return (doc, worst, frames);
        }
        // Stand in for the frame this loop is pretending to be. `take_capture`
        // polls *without* blocking — the whole point — so a loop with nothing
        // else to do would otherwise spin through every iteration before the
        // GPU had finished the first copy, and conclude the capture had hung.
        // Deliberately outside the timing above: it is the test's cost, not the
        // capture's.
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    panic!(
        "the capture never came home in {frames} frames — it has stopped making \
         progress rather than merely being slow, since the budget is far past \
         what any real capture needs"
    );
}

#[test]
fn a_capture_reads_back_exactly_what_the_blocking_path_does() {
    // The whole point of the non-blocking path is that it produces the *same*
    // picture. If it can drift from `read_layer_rect` and `export_rgba`, an
    // autosave becomes a second reading of the canvas and a file that quietly
    // disagrees with the screen.
    let mut h = harness_or_skip!();
    h.canvas.ensure_slots(&h.gpu.device, &h.gpu.queue, 2);

    h.fill(0, Color::from_srgb_u8(200, 40, 40, 255));
    h.stamp(&[dab(20.0, 20.0, 10.0, 1.0)]);
    h.commit_to(
        1,
        Color::from_srgb_u8(30, 60, 220, 255),
        0.5,
        BrushMode::Paint,
    );

    let draws = vec![
        LayerDraw {
            slot: 0,
            opacity: 1.0,
            blend: 0,
            visible: true,
            mask: None,
            clipped: false,
        },
        LayerDraw {
            slot: 1,
            opacity: 1.0,
            blend: 0,
            visible: true,
            mask: None,
            clipped: false,
        },
    ];
    let full = PixelRect {
        x: 0,
        y: 0,
        width: DOC,
        height: DOC,
    };
    let expected: Vec<Vec<u8>> = (0..2)
        .map(|slot| {
            h.canvas
                .read_layer_rect(&h.gpu.device, &h.gpu.queue, slot, full)
        })
        .collect();
    let expected_merged = h.canvas.export_rgba(&h.gpu.device, &h.gpu.queue, &draws);

    let (captured, _, _) = run_capture(h.gpu, &mut h.canvas, &[0, 1], &draws);

    assert_eq!(captured.size, UVec2::splat(DOC));
    assert_eq!(captured.layers.len(), 2);
    assert_eq!(captured.layers[0], expected[0], "layer 0 differs");
    assert_eq!(captured.layers[1], expected[1], "layer 1 differs");
    assert_eq!(
        captured.merged, expected_merged,
        "the flattened preview differs from the export"
    );
}

#[test]
fn a_document_too_large_for_one_staging_buffer_is_read_back_in_bands() {
    // `downlevel_defaults` caps a buffer at 256 MB, which a 10000² canvas —
    // 400 MB of RGBA — sails past. `create_buffer` refuses the size and the
    // validation error aborts the process, which is exactly how a real
    // document died on its first undo capture. Every readback therefore goes a
    // band of rows at a time.
    //
    // Driven here by lowering the limit rather than by making a document that
    // reaches the real one: an 8192² canvas is a gigabyte of GPU memory to ask
    // a CI runner for, and an untested path that only the largest documents
    // take is precisely the one that returns a sheared picture in silence.
    let mut h = harness_or_skip!();
    h.canvas.ensure_slots(&h.gpu.device, &h.gpu.queue, 2);

    h.fill(0, Color::from_srgb_u8(200, 40, 40, 255));
    h.stamp(&[dab(20.0, 20.0, 10.0, 1.0)]);
    h.commit_to(
        1,
        Color::from_srgb_u8(30, 60, 220, 255),
        0.5,
        BrushMode::Paint,
    );

    let draws = vec![
        LayerDraw {
            slot: 0,
            opacity: 1.0,
            blend: 0,
            visible: true,
            mask: None,
            clipped: false,
        },
        LayerDraw {
            slot: 1,
            opacity: 1.0,
            blend: 0,
            visible: true,
            mask: None,
            clipped: false,
        },
    ];
    let full = PixelRect {
        x: 0,
        y: 0,
        width: DOC,
        height: DOC,
    };

    // The truth, read in one go the way every ordinary document is.
    let whole_layer = h
        .canvas
        .read_layer_rect(&h.gpu.device, &h.gpu.queue, 0, full);
    let whole_export = h.canvas.export_rgba(&h.gpu.device, &h.gpu.queue, &draws);

    // Seven bands of ten rows and a last band of four, so the short final band
    // and the reused buffer are both exercised. A limit that divided the
    // document evenly would leave the interesting case untested.
    let row = (DOC * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    h.canvas.set_readback_limit((row * 10) as u64);

    let banded_layer = h
        .canvas
        .read_layer_rect(&h.gpu.device, &h.gpu.queue, 0, full);
    assert_eq!(
        banded_layer, whole_layer,
        "the banded undo readback differs from the single-buffer one"
    );

    let banded_export = h.canvas.export_rgba(&h.gpu.device, &h.gpu.queue, &draws);
    assert_eq!(
        banded_export, whole_export,
        "the banded export differs from the single-buffer one"
    );

    // And the capture, which bands *across frames* — the hard half, because a
    // step is then several buffers and the flattened preview has to survive
    // between them.
    let (captured, _, _) = run_capture(h.gpu, &mut h.canvas, &[0, 1], &draws);
    assert_eq!(captured.size, UVec2::splat(DOC));
    assert_eq!(
        captured.layers[0], whole_layer,
        "a banded capture's layer differs"
    );
    assert_eq!(
        captured.merged, whole_export,
        "a banded capture's flattened preview differs"
    );
}

/// **A banded write puts the same pixels down as an unbanded one.**
///
/// `write_layer_rect` now bands for the reason every readback here does: a
/// canvas-sized `write_texture` asks the device for a canvas-sized staging
/// buffer, which is 400 MB on a 10000² document against the 256 MB
/// `downlevel_defaults` allows, and a failed staging allocation is fatal rather
/// than catchable. The reader was banded and the writer was not.
///
/// What can go wrong in the *arithmetic* is what this measures: the same bytes
/// are written twice, once whole and once in five bands, and the two slices
/// have to come back identical. A band written to the wrong row, a source
/// offset stepping by the padded stride instead of the tight one, or a last
/// band sized from the wrong end all show up here and in nothing else. Driven
/// by lowering the limit, exactly as the readback above is, because reaching
/// the real one needs a canvas no CI runner can hold.
///
/// **It does not cover the submit or the wait, which are the point of the
/// change**, and saying so is better than letting the name imply otherwise:
/// delete both `queue.submit([])` and the `device.poll` and this stays green,
/// because the pixels are identical either way. What those two lines bound is
/// how much staging is alive at once, which is a property of the allocator and
/// not of any picture — there is nothing to read back and no adapter-independent
/// figure to assert. They are held by the argument at `write_layer_rect`
/// instead, and by there being one place that writes a band.
///
/// The rectangle is deliberately awkward — off the origin, an odd width whose
/// row is not a multiple of the copy alignment, and a height that leaves a
/// short final band. A rectangle that divided evenly would leave the case that
/// actually breaks untested.
#[test]
fn a_banded_layer_write_lands_exactly_where_an_unbanded_one_does() {
    let mut h = harness_or_skip!();
    h.canvas.ensure_slots(&h.gpu.device, &h.gpu.queue, 2);

    let rect = PixelRect {
        x: 5,
        y: 7,
        width: 53,
        height: 41,
    };
    // Every pixel different, so a band landing one row out is a mismatch rather
    // than a coincidence.
    let pixels: Vec<u8> = (0..(rect.area() * 4) as usize)
        .map(|i| (i * 31 + 7) as u8)
        .collect();

    // The truth: one `write_texture`, at the device's own limit.
    h.canvas
        .write_layer_rect(&h.gpu.device, &h.gpu.queue, 0, rect, &pixels);
    let whole = h
        .canvas
        .read_layer_rect(&h.gpu.device, &h.gpu.queue, 0, rect);

    let padded = (rect.width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let rows = 9;
    assert!(
        rows < rect.height && !rect.height.is_multiple_of(rows),
        "the fixture has to band, and to leave a short last band"
    );
    h.canvas.set_readback_limit((padded * rows) as u64);

    h.canvas
        .write_layer_rect(&h.gpu.device, &h.gpu.queue, 1, rect, &pixels);
    // Read through the same banded path on both sides of the comparison would
    // hide a read bug; that one is already pinned by the test above, so this
    // reads slot 1 banded and re-reads slot 0 the same way. Slot 0's bytes were
    // put down whole, so the two differing can only be the *write*.
    let banded = h
        .canvas
        .read_layer_rect(&h.gpu.device, &h.gpu.queue, 1, rect);
    let whole_reread = h
        .canvas
        .read_layer_rect(&h.gpu.device, &h.gpu.queue, 0, rect);

    assert_eq!(whole_reread, whole, "the banded read moved the truth");
    assert_eq!(
        banded, whole,
        "a banded layer write put different pixels down"
    );
}

#[test]
fn a_capture_of_a_large_document_never_costs_a_frame() {
    // The measurement the whole feature rests on. A save's blocking readback is
    // one `poll(wait)` per layer, which on a full 2048-square stack is tens of
    // milliseconds of the main thread doing nothing — fine once, at pointer-up,
    // and exactly wrong on a timer. The capture records a copy, polls without
    // waiting, and reads the result back four megabytes at a time, so what any
    // one frame pays is bounded by the chunk rather than by the document.
    //
    // The bound is deliberately loose — half a frame, against about a
    // millisecond measured. It is not a claim about this machine's speed; it is
    // a claim that nothing on this path waits for the GPU and nothing on it
    // scales with the size of the document, and either failing would put this
    // far past it on any adapter, software rasterisers included.
    let mut h = harness_or_skip!();
    const BIG: u32 = 2048;
    const LAYERS: u32 = 8;

    h.canvas.resize(
        &h.gpu.device,
        &h.gpu.queue,
        UVec2::splat(BIG),
        Anchor::Centre,
        1,
    );
    h.canvas.ensure_slots(&h.gpu.device, &h.gpu.queue, LAYERS);
    let enc = h.encoder();
    h.canvas.clear_all_layers(&h.gpu.queue);
    h.gpu.queue.submit(Some(enc.finish()));

    let slots: Vec<u32> = (0..LAYERS).collect();
    let draws: Vec<LayerDraw> = slots
        .iter()
        .map(|slot| LayerDraw {
            slot: *slot,
            opacity: 1.0,
            blend: 0,
            visible: true,
            mask: None,
            clipped: false,
        })
        .collect();

    let started = std::time::Instant::now();
    let (captured, worst, frames) = run_capture(h.gpu, &mut h.canvas, &slots, &draws);
    let total = started.elapsed();

    assert_eq!(captured.layers.len(), LAYERS as usize);
    assert_eq!(
        captured.layers[0].len(),
        (BIG * BIG * 4) as usize,
        "a captured layer must be the whole canvas"
    );
    assert_eq!(captured.merged.len(), (BIG * BIG * 4) as usize);

    let software = h.gpu.adapter.get_info().device_type == wgpu::DeviceType::Cpu;
    eprintln!(
        "capture of {BIG}x{BIG}, {LAYERS} layers: {frames} frames, worst frame \
         {:.2} ms, {:.0} ms end to end{}",
        worst.as_secs_f64() * 1000.0,
        total.as_secs_f64() * 1000.0,
        if software { " (software adapter)" } else { "" },
    );

    // The guarantee, asserted everywhere: the work is *spread*. One frame per
    // layer is the floor the design implies — a capture that finished in fewer
    // frames than it has layers would have to have read more than one layer in
    // some frame, which is the thing being ruled out.
    //
    // This is the assertion that survives the machine changing under it. It
    // holds on a software rasteriser at forty milliseconds a frame exactly as
    // it does on a discrete card at one.
    assert!(
        frames >= LAYERS as usize,
        "the capture finished in {frames} frames for {LAYERS} layers, so some \
         frame read more than one — the whole point is that it does not",
    );

    // And the wall clock — on a machine whose wall clock means something.
    //
    // Not in CI, and this is not a threshold that wants tuning. The 0.0.2
    // release build failed here twice and the two failures are the argument:
    // the Linux runners have no GPU at all and took 47 ms a frame, because a
    // software rasteriser produces the pixels on the CPU; the macOS runner has
    // a real GPU and took 8.48 ms, which is not a regression, it is a shared
    // virtual machine missing an 8 ms budget by half a millisecond. Numbers
    // from hardware nobody chose, under a load nobody controls, are not
    // evidence about this code. Chasing them upward until CI is quiet would end
    // with a budget so loose it could not catch the thing it exists to catch.
    //
    // So the timing is *reported* everywhere and *asserted* where somebody is
    // actually painting: 8 ms is the figure that catches a blocking readback
    // creeping back onto this path, and a developer who reintroduces one sees
    // it on the machine they reintroduced it on. What protects the branch is
    // the structural assertion above, which is machine-independent and is the
    // real guarantee anyway. The GPU tests already skip rather than fail when
    // there is no adapter; this is the same idea one step further in.
    if !software && std::env::var_os("CI").is_none() {
        assert!(
            worst < std::time::Duration::from_millis(8),
            "one frame of the capture cost {:.2} ms — something on this path is \
             waiting for the GPU, or reading the whole document at once",
            worst.as_secs_f64() * 1000.0,
        );
    }
}

#[test]
fn a_second_capture_is_refused_while_one_is_in_flight() {
    // Two at once would double the staging cost of a job that is going to be
    // repeated in five minutes anyway, and the caller has no way to tell the
    // two results apart.
    let mut h = harness_or_skip!();
    let draws = vec![LayerDraw {
        slot: 0,
        opacity: 1.0,
        blend: 0,
        visible: true,
        mask: None,
        clipped: false,
    }];
    assert!(h.canvas.begin_capture(&[0], &draws));
    assert!(
        !h.canvas.begin_capture(&[0], &draws),
        "a second capture was accepted"
    );
    assert!(h.canvas.capture_in_flight());

    drive_to_completion(h.gpu, &mut h.canvas);
    assert!(!h.canvas.capture_in_flight());
    assert!(
        h.canvas.begin_capture(&[0], &draws),
        "the slot was never freed"
    );
}

#[test]
fn a_cancelled_capture_hands_its_buffers_back_rather_than_being_dropped() {
    // The failure this avoids is the one `reset_probes` documents: dropping a
    // buffer whose `map_async` is outstanding, or handing it straight to the
    // next job, is a validation error — and a validation error aborts the
    // process. So a cancelled capture stays where it is until the GPU has
    // finished with it, and then really does go, or the next autosave never
    // starts.
    let mut h = harness_or_skip!();
    let draws = vec![LayerDraw {
        slot: 0,
        opacity: 1.0,
        blend: 0,
        visible: true,
        mask: None,
        clipped: false,
    }];
    assert!(h.canvas.begin_capture(&[0], &draws));

    let mut enc = h.encoder();
    h.canvas
        .drive_capture(&h.gpu.device, &h.gpu.queue, &mut enc);
    h.gpu.queue.submit(Some(enc.finish()));
    h.canvas.submit_capture();
    h.canvas.cancel_capture();

    for _ in 0..2000 {
        assert!(
            h.canvas.take_capture(&h.gpu.device).is_none(),
            "a cancelled capture must not produce a document"
        );
        if !h.canvas.capture_in_flight() {
            break;
        }
        // As in `drive_to_completion`: the poll does not wait, so this loop has
        // to.
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(
        !h.canvas.capture_in_flight(),
        "the cancelled capture never settled"
    );
    assert!(
        h.canvas.begin_capture(&[0], &draws),
        "the slot stayed taken"
    );
}

// --- floating transforms ----------------------------------------------------

/// Everything the transform tool asks of the GPU, in the order it asks it.
///
/// The offset is a whole number of pixels on purpose: the resampler's bilinear
/// taps then land exactly on texel centres, so the moved block is byte for byte
/// the block that was lifted and the assertions can be exact rather than
/// approximate. Filtering itself is the sampler's and is not what this is for —
/// what it is for is that the picture lands where `Transform` said it would,
/// that the layer is untouched until the commit, and that one undo patch puts
/// back both the hole and the place it went.
#[test]
fn a_transform_lands_where_the_maths_says_and_undo_restores_both_ends() {
    let mut h = harness_or_skip!();
    let block = PixelRect {
        x: 10,
        y: 10,
        width: 10,
        height: 10,
    };
    let red = [220, 40, 30, 255];
    h.write_block(0, block, red);
    assert_eq!(h.pixel(12, 12), red, "the block did not go in");

    let mut xf = Transform::identity(block);
    let preview = h
        .canvas
        .begin_float(
            &h.gpu.device,
            &h.gpu.queue,
            1,
            &FloatSource {
                slot: 0,
                rect: block,
                pixels: None,
                mask: None,
            },
        )
        .expect("no room for a preview");
    assert_eq!(h.canvas.float_preview(), Some((0, preview)));

    // Picking the pixels up leaves a hole in the *preview*, and leaves the
    // layer exactly as it was: nothing is written until the commit, which is
    // what makes abandoning a transform free and what makes the undo patch
    // captured at commit time the pre-transform pixels.
    assert_eq!(h.pixel_in(preview, 12, 12), [0, 0, 0, 0], "no hole");
    assert_eq!(h.pixel(12, 12), red, "the layer was written to early");

    xf.offset = Vec2::splat(20.0);
    let params = FloatParams {
        inverse: xf.inverse(),
        dest: xf.dest_rect(UVec2::splat(DOC)),
    };
    let mut enc = h.encoder();
    h.canvas.draw_float(&h.gpu.queue, &mut enc, &params);
    h.gpu.queue.submit(Some(enc.finish()));

    assert_eq!(h.pixel_in(preview, 32, 32), red, "the preview did not move");
    assert_eq!(h.pixel_in(preview, 12, 12), [0, 0, 0, 0], "the hole filled");
    assert_eq!(h.pixel(32, 32), [0, 0, 0, 0], "the layer moved too early");

    // Commit, capturing undo first exactly as `finish_stroke` does.
    let damage = xf.damage(UVec2::splat(DOC), true).expect("something to do");
    let before = h
        .canvas
        .read_layer_rect(&h.gpu.device, &h.gpu.queue, 0, damage);
    let mut enc = h.encoder();
    h.canvas
        .commit_float(&h.gpu.queue, &mut enc, damage, &params);
    h.gpu.queue.submit(Some(enc.finish()));
    h.canvas.end_float(&h.gpu.queue);
    assert_eq!(h.canvas.float_preview(), None);

    assert_eq!(h.pixel(32, 32), red, "the commit did not land");
    assert_eq!(h.pixel(12, 12), [0, 0, 0, 0], "the hole was not committed");

    // One patch, both ends. A patch covering only where the pixels went would
    // undo to a document that still had the hole in it.
    h.canvas
        .write_layer_rect(&h.gpu.device, &h.gpu.queue, 0, damage, &before);
    assert_eq!(h.pixel(12, 12), red, "undo did not restore the source");
    assert_eq!(h.pixel(32, 32), [0, 0, 0, 0], "undo left the copy behind");
}

/// A transform with a selection in hand moves what the selection covers and
/// leaves the rest of the layer alone — both the pixels it takes and the hole
/// it leaves.
#[test]
fn a_transform_moves_only_what_the_selection_covers() {
    let mut h = harness_or_skip!();
    let block = PixelRect {
        x: 10,
        y: 10,
        width: 20,
        height: 20,
    };
    let red = [220, 40, 30, 255];
    h.write_block(0, block, red);

    // The top-left quarter of the block.
    let selection = Selection::rectangle(Vec2::splat(10.0), Vec2::splat(20.0), UVec2::splat(DOC))
        .expect("a selection");
    let source = selection.bounds();
    let mut xf = Transform::identity(source);
    let preview = h
        .canvas
        .begin_float(
            &h.gpu.device,
            &h.gpu.queue,
            1,
            &FloatSource {
                slot: 0,
                rect: source,
                pixels: None,
                mask: Some(&selection),
            },
        )
        .expect("no room for a preview");

    xf.offset = Vec2::new(30.0, 0.0);
    let params = FloatParams {
        inverse: xf.inverse(),
        dest: xf.dest_rect(UVec2::splat(DOC)),
    };
    let mut enc = h.encoder();
    h.canvas.draw_float(&h.gpu.queue, &mut enc, &params);
    h.gpu.queue.submit(Some(enc.finish()));

    assert_eq!(h.pixel_in(preview, 12, 12), [0, 0, 0, 0], "no hole");
    assert_eq!(h.pixel_in(preview, 42, 12), red, "the copy did not arrive");
    assert_eq!(
        h.pixel_in(preview, 25, 25),
        red,
        "the unselected part of the block was taken too"
    );
}

// --- a lift is a complement -------------------------------------------------

/// Lift `sel`'s bounding rectangle out of slot 0, carry it `by` and put it
/// down, exactly as the transform tool does. Returns the destination rectangle.
fn lift_and_move(h: &mut Harness, sel: &Selection, by: Vec2) -> PixelRect {
    let source = sel.bounds();
    let mut xf = Transform::identity(source);
    h.canvas
        .begin_float(
            &h.gpu.device,
            &h.gpu.queue,
            1,
            &FloatSource {
                slot: 0,
                rect: source,
                pixels: None,
                mask: Some(sel),
            },
        )
        .expect("no room for a preview");

    xf.offset = by;
    let params = FloatParams {
        inverse: xf.inverse(),
        dest: xf.dest_rect(UVec2::splat(DOC)),
    };
    let damage = xf.damage(UVec2::splat(DOC), true).expect("something to do");
    let mut enc = h.encoder();
    h.canvas.draw_float(&h.gpu.queue, &mut enc, &params);
    h.canvas
        .commit_float(&h.gpu.queue, &mut enc, damage, &params);
    h.gpu.queue.submit(Some(enc.finish()));
    h.canvas.end_float(&h.gpu.queue);

    PixelRect {
        x: (source.x as f32 + by.x) as u32,
        y: (source.y as f32 + by.y) as u32,
        ..source
    }
}

fn read_rect(h: &Harness, slot: u32, rect: PixelRect) -> Vec<u8> {
    h.canvas
        .read_layer_rect(&h.gpu.device, &h.gpu.queue, slot, rect)
}

/// The alpha channel alone, which is the one that adds: it is linear 8-bit even
/// in an sRGB format, so a complement can be stated on it exactly where two
/// gamma-encoded colours could only be compared.
fn alphas(px: &[u8]) -> Vec<u8> {
    px.iter().skip(3).step_by(4).copied().collect()
}

/// The bug the whole of this section exists for: paint inside a selection, move
/// it away, and a one-pixel ghost of the outline was left behind at the source.
///
/// The lift used to scale the layer by the selection's coverage — but painting
/// is *already* clipped by that same coverage, so a half-covered pixel holding
/// half a stroke's alpha had the mask applied to it twice: a quarter went into
/// the float and a quarter stayed on the layer. Every antialiased pixel of the
/// boundary kept a share of the paint, which is exactly a faint tracing of
/// where the selection had been.
///
/// Stated as a complement rather than as a value: everything that was there is
/// now at the destination, and the source is back to what it held before a
/// stroke ever went near it. Neither half is a number anybody worked out by
/// hand, so the test says nothing about the falloff except that it moved whole.
#[test]
fn a_lift_leaves_no_ghost_of_the_selection_it_was_painted_through() {
    let mut h = harness_or_skip!();

    // A triangle, so the boundary runs through every coverage between 0 and 255
    // rather than through the one exact half a fractional rectangle gives.
    let sel = Selection::polygon(
        &[
            Vec2::new(8.0, 8.0),
            Vec2::new(28.0, 12.0),
            Vec2::new(14.0, 28.0),
        ],
        UVec2::splat(DOC),
    )
    .expect("a selection");
    let source = sel.bounds();
    let empty = read_rect(&h, 0, source);
    assert!(
        empty.iter().all(|b| *b == 0),
        "the layer did not start bare"
    );

    // Paint through it, as the artist did. The dab covers the whole bounding
    // rectangle, so what shapes the mark is the selection and nothing else.
    h.set_selection(Some(sel.clone()));
    h.stamp(&[dab(18.0, 18.0, 30.0, 1.0)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);
    let painted = read_rect(&h, 0, source);
    assert!(
        alphas(&painted).iter().any(|a| *a > 0 && *a < 255),
        "the selection's edge did not come out antialiased, so this test would \
         pass on a hard-edged mask that never had the bug"
    );

    let dest = lift_and_move(&mut h, &sel, Vec2::new(0.0, 32.0));

    assert_eq!(
        read_rect(&h, 0, source),
        empty,
        "a ghost of the selection was left where the paint was lifted from"
    );
    // **The alpha, within a level, and not the bytes.**
    //
    // What this test is for is conservation: the float has to carry the paint
    // the layer gave up, or the ghost is back in the other direction. That
    // property lives in the alpha — see `alphas`, which is linear eight bits
    // even in an sRGB format — and it is computed by a shader in floating point
    // and stored twice through eight bits, so it is exact only to the level the
    // store has. It used to be `assert_eq!` on the raw bytes, which is a
    // promise about *rounding on one particular device*: it held on the
    // hardware it was written on and failed on the software rasteriser CI runs,
    // by one level of alpha at the antialiased edge, on a tag that had already
    // been pushed. `UMBER_TEST_SOFTWARE=1` is how that is now found first.
    //
    // The exact claim has not been given up, it is made where it can be kept:
    // `a_hard_edged_rectangular_lift_is_exact` asserts the whole rectangle byte
    // for byte, colour included, over a mask whose coverage is only ever 0 or
    // 1 — where the arithmetic has nothing to round. Here the colour is
    // deliberately not compared at all: at an alpha of two the stored colour is
    // a steep function of that alpha, so a single level of it moves the encoded
    // byte by six, which says nothing about the lift and everything about
    // dividing by a small number.
    let carried = alphas(&read_rect(&h, 0, dest));
    let gave_up = alphas(&painted);
    assert_eq!(
        carried.len(),
        gave_up.len(),
        "the rectangles differ in size"
    );
    let worst = carried
        .iter()
        .zip(&gave_up)
        .enumerate()
        .max_by_key(|(_, (c, g))| c.abs_diff(**g))
        .map(|(i, (c, g))| (i, *c, *g));
    if let Some((at, carried, gave_up)) = worst {
        assert!(
            carried.abs_diff(gave_up) <= 1,
            "the float did not carry every pixel the layer gave up: at pixel \
             {at} the layer gave up {gave_up} and the float carries {carried}, \
             which is {} levels and not the one the store can round by",
            carried.abs_diff(gave_up),
        );
    }
}

/// A lift through a **feather**, over content the selection did not make.
///
/// The test above paints *through* the selection it then lifts through, so
/// `a == m` everywhere and the share `min(a, m) / a` is identically one: the
/// float takes all of it and the hole is zero by construction. That is the
/// right shape for the ghost it exists to catch, and it is deliberately not a
/// test of the arithmetic — it would pass under any rule that reduces to one
/// when `a <= m`.
///
/// Here the layer is filled to a flat alpha the selection knows nothing about,
/// so `a` and `m` are independent and the share runs over its whole range: one
/// where the feather's ramp is above the alpha, `m / a` where it is below.
/// What is asserted is therefore **conservation** rather than "the float
/// carries all of it" — what leaves plus what stays is what was there — which
/// is the one number `transform.wgsl` drives both passes from, and it is the
/// same claim `a_cut_takes_exactly_what_it_leaves_behind` makes on the CPU
/// side.
#[test]
fn a_lift_through_a_feathered_selection_splits_the_alpha_it_finds() {
    let mut h = harness_or_skip!();

    let sel = Selection::polygon(
        &[
            Vec2::new(12.0, 10.0),
            Vec2::new(28.0, 14.0),
            Vec2::new(16.0, 26.0),
        ],
        UVec2::splat(DOC),
    )
    .expect("a selection")
    .feathered(4.0, UVec2::splat(DOC))
    .expect("a soft selection");
    let source = sel.bounds();

    // A flat half-transparent block, written straight into the layer rather
    // than painted through anything — which is the whole of what makes `a`
    // independent of the mask here. Premultiplied, as a layer holds it.
    let before = 128u8;
    h.write_block(0, source, [64, 30, 12, before]);

    let dest = lift_and_move(&mut h, &sel, Vec2::new(0.0, 30.0));

    let left = alphas(&read_rect(&h, 0, source));
    let taken = alphas(&read_rect(&h, 0, dest));
    assert_eq!(left.len(), taken.len(), "the rectangles differ in size");

    let mut split = 0;
    for (i, (l, t)) in left.iter().zip(&taken).enumerate() {
        let sum = u16::from(*l) + u16::from(*t);
        // Two levels rather than one: this is two stores of two numbers a
        // shader worked out from one, where the antialiased test compares a
        // single stored value against a single stored value.
        assert!(
            sum.abs_diff(u16::from(before)) <= 2,
            "at pixel {i} the layer kept {l} and the float took {t}, which is \
             {sum} of the {before} that was there",
        );
        if *l > 0 && *t > 0 {
            split += 1;
        }
    }
    // And the feather is what is being tested: a hard mask splits no pixel at
    // all and an antialiased edge splits a one-pixel band, so a ramp crossing
    // the block's own alpha has to split a great many or this is the test above
    // wearing a different name.
    assert!(
        split > 40,
        "only {split} pixels were partly lifted, so the feather is not being \
         exercised"
    );
}

/// The cheap case, and the one that should be exact on both axes: an integer
/// rectangle, whose coverage is only ever 0 or 255. Here the lift takes the
/// selected pixels whole and leaves the rest of the layer untouched — the
/// property `a_transform_moves_only_what_the_selection_covers` checks a corner
/// of, stated over every pixel of the rectangle.
#[test]
fn a_hard_edged_rectangular_lift_is_exact() {
    let mut h = harness_or_skip!();

    let block = PixelRect {
        x: 8,
        y: 8,
        width: 24,
        height: 24,
    };
    let red = [220, 40, 30, 255];
    h.write_block(0, block, red);

    let sel = Selection::rectangle(
        Vec2::new(12.0, 12.0),
        Vec2::new(28.0, 28.0),
        UVec2::splat(DOC),
    )
    .expect("a selection");
    let source = sel.bounds();
    let before = read_rect(&h, 0, source);

    let dest = lift_and_move(&mut h, &sel, Vec2::new(0.0, 32.0));

    let kept = read_rect(&h, 0, source);
    let moved = read_rect(&h, 0, dest);
    assert!(kept.iter().all(|b| *b == 0), "the hole is not clean");
    assert_eq!(moved, before, "the block did not arrive whole");
    // The corner of the block outside the selection is nobody's business but
    // the layer's: a lift that reached past its mask would take it too.
    assert_eq!(
        h.pixel(10, 10),
        red,
        "the lift reached outside the selection"
    );
}

/// **A float drawn at the exact identity comes back byte for byte, including
/// its partly covered pixels, on a canvas whose size is not a power of two.**
///
/// This is the load-bearing GPU property under `docs/text-tool.md` §4(c), and
/// nothing needed it until now. Re-rasterising text through the transform means
/// the floating texture holds pixels *already* scaled and turned, so
/// `FloatParams::inverse` is `Affine::IDENTITY` and `fs_sample` becomes a blit.
/// The whole plan rests on that blit changing nothing — if it costs even a level
/// of alpha per frame, then a drag is a picture degrading as it is dragged, which
/// is worse than the resample it replaces because it never settles.
///
/// It is not covered by `a_hard_edged_rectangular_lift_is_exact`, twice over.
/// That one moves by a whole-pixel translation rather than the identity, and its
/// block is **opaque**, so it says nothing about a partly covered pixel — which
/// is every edge of every glyph and the only thing a resample visibly harms.
///
/// **The canvas is 100 square deliberately.** `fs_sample` divides the document
/// point by `doc_size` to get a texture coordinate and the sampler multiplies it
/// back by the size, and the fragment centre is at `pixel + 0.5`, which lands on
/// a texel centre exactly. For a power-of-two size both the divide and the
/// multiply are exact in binary and the tap is provably the one texel; for 100
/// they are not, so a coordinate can come back a unit in the last place off its
/// texel centre and the bilinear filter picks up a sliver of a neighbour. **No
/// float or sampler test in this file had ever used a non-power-of-two canvas** —
/// they are 64 and 512, and the three `resize` calls to 128, 32 and 96 are about
/// resizing rather than about sampling — so the only sizes this path was ever
/// tested at were the ones where the arithmetic cannot fail. Umber's canvases are
/// any size at all.
#[test]
fn a_float_drawn_at_the_identity_is_an_exact_blit_of_its_own_pixels() {
    let Some(gpu) = shared_gpu() else {
        eprintln!("no adapter; skipping");
        return;
    };
    static SERIAL: Mutex<()> = Mutex::new(());
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    for side in [100u32, 64] {
        let mut canvas = CanvasRenderer::new(
            &gpu.device,
            &gpu.queue,
            UVec2::splat(side),
            TARGET_FORMAT,
            1,
        );
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        canvas.clear_all_layers(&gpu.queue);
        canvas.clear_stroke(&gpu.device, &mut enc);
        gpu.queue.submit(Some(enc.finish()));

        // Layer-texture form, and **every pixel of it is exactly valid
        // premultiplied without needing the encoder to say so**, which is what
        // lets the assertion below be about bytes. Two kinds, alternating by row:
        //
        // * an opaque pixel, where premultiplied and straight are the same bytes
        //   whatever the colour, so the colour channels carry real information;
        // * a black pixel at a partial coverage, where the premultiplied colour
        //   is zero in every space there is, so the alpha can run over every
        //   level a coverage byte takes.
        //
        // Between them they cover the colour and the fringe. `docimport::srgb`'s
        // encoder is the honest way to build a coloured fringe and is
        // `pub(crate)`; asking for it to be opened up for a fixture would be
        // widening an interface to make a test easier, and these bytes make the
        // same claim.
        //
        // The coverage runs high-frequency along each row on purpose: a tap that
        // strayed even a unit in the last place off its texel centre would pick
        // up a neighbour several levels away, where a smooth ramp would hide it.
        let rect = PixelRect {
            x: 7,
            y: 11,
            width: 51,
            height: 33,
        };
        let pixels: Vec<u8> = (0..rect.area())
            .flat_map(|i| {
                let (x, y) = (i % u64::from(rect.width), i / u64::from(rect.width));
                if y % 2 == 0 {
                    [(x * 37 % 256) as u8, (x * 91 % 256) as u8, 17, 255]
                } else {
                    [0, 0, 0, (x * 149 % 256) as u8]
                }
            })
            .collect();
        assert!(
            pixels.chunks_exact(4).any(|p| (1..255).contains(&p[3])),
            "the fixture has no partly covered pixel, so this test would pass on \
             a blit that only handled opaque ones"
        );

        let preview = canvas
            .begin_float(
                &gpu.device,
                &gpu.queue,
                1,
                &FloatSource {
                    slot: 0,
                    rect,
                    pixels: Some(&pixels),
                    mask: None,
                },
            )
            .expect("no room for a preview");

        // The identity, and the destination the pixels are already at. This is
        // exactly what a text float would pass every frame: the map went into the
        // rasteriser, so there is nothing left for the sampler to do.
        let params = FloatParams {
            inverse: Transform::identity(rect).inverse(),
            dest: Some(rect),
        };

        // One frame, one encoder, one submit.
        //
        // **There is deliberately no loop here, and an earlier draft's was worth
        // removing twice over.** It drew the preview three times to claim it was
        // showing that "a drag does not rot the picture frame by frame", and it
        // could not show that: `render_float` restores the damaged rectangle out
        // of `float.base` *before* it draws, so repeating it with the same params
        // is idempotent by construction and three iterations cannot fail where one
        // passes. It also broke `draw_float`'s own documented rule — one uniform
        // write per encoder, because `Queue::write_buffer` is flushed ahead of the
        // encoder's commands, so all three passes read the last write. Benign only
        // because the three writes were identical, which is the same fact that
        // made the loop vacuous.
        //
        // What a real multi-frame drag would need is an encoder and a submit per
        // frame with a *different* matrix each time, which is a different test
        // and would be about `span()`'s restore rather than about the blit.
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        canvas.draw_float(&gpu.queue, &mut enc, &params);
        gpu.queue.submit(Some(enc.finish()));
        assert_eq!(
            canvas.read_layer_rect(&gpu.device, &gpu.queue, preview, rect),
            pixels,
            "on a {side}-square canvas the previewed identity blit moved the \
             pixels it was handed, so re-rasterising text through the transform \
             would degrade the picture it was supposed to keep sharp"
        );

        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        canvas.commit_float(&gpu.queue, &mut enc, rect, &params);
        gpu.queue.submit(Some(enc.finish()));
        canvas.end_float(&gpu.queue);
        assert_eq!(
            canvas.read_layer_rect(&gpu.device, &gpu.queue, 0, rect),
            pixels,
            "on a {side}-square canvas the committed identity blit is not the \
             pixels it was handed"
        );
    }
}

/// A selection dragged off the edge of the canvas. Its bounding rectangle is
/// clamped, so the mask's own rectangle now shares a border with the document —
/// which is where `fs_mask`'s arithmetic decision about "outside the mask" and
/// `fs_sample`'s about "outside the canvas" both have to hold at once.
#[test]
fn a_selection_running_off_the_canvas_lifts_as_cleanly_as_one_inside_it() {
    let mut h = harness_or_skip!();

    let sel = Selection::rectangle(
        Vec2::new(-6.5, 10.5),
        Vec2::new(20.5, 30.5),
        UVec2::splat(DOC),
    )
    .expect("a selection");
    let source = sel.bounds();
    assert_eq!(
        source.x, 0,
        "the selection was expected to reach the border"
    );
    let empty = read_rect(&h, 0, source);

    h.set_selection(Some(sel.clone()));
    h.stamp(&[dab(10.0, 20.0, 34.0, 1.0)]);
    h.commit(Color::WHITE, 1.0, BrushMode::Paint);
    let painted = read_rect(&h, 0, source);

    let dest = lift_and_move(&mut h, &sel, Vec2::new(0.0, 32.0));

    assert_eq!(
        read_rect(&h, 0, source),
        empty,
        "a ghost was left along a selection that ran off the canvas"
    );
    assert_eq!(
        read_rect(&h, 0, dest),
        painted,
        "the float dropped pixels at the border"
    );
}

/// The other half of the fix, and the reason it is a `min` rather than "take
/// everything the selection touches": paint the selection did **not** make must
/// still be split between the hole and the float, with the selection's own
/// antialiasing on both sides of the cut.
///
/// The complement is stated on alpha, which is linear 8-bit even in an sRGB
/// format and therefore actually adds.
#[test]
fn a_lift_still_splits_paint_the_selection_did_not_make() {
    let mut h = harness_or_skip!();

    let block = PixelRect {
        x: 4,
        y: 4,
        width: 32,
        height: 24,
    };
    h.write_block(0, block, [220, 40, 30, 255]);

    // The boundary falls down the middle of column 20 and of row 20.
    let sel = Selection::rectangle(
        Vec2::new(8.5, 8.5),
        Vec2::new(20.5, 20.5),
        UVec2::splat(DOC),
    )
    .expect("a selection");
    let source = sel.bounds();
    let before = alphas(&read_rect(&h, 0, source));

    let dest = lift_and_move(&mut h, &sel, Vec2::new(0.0, 32.0));

    let kept = alphas(&read_rect(&h, 0, source));
    let moved = alphas(&read_rect(&h, 0, dest));
    for (i, was) in before.iter().enumerate() {
        let sum = u16::from(kept[i]) + u16::from(moved[i]);
        assert!(
            sum.abs_diff(u16::from(*was)) <= 1,
            "pixel {i}: the hole kept {} and the float took {}, which is not \
             the {was} that was there",
            kept[i],
            moved[i]
        );
    }
    // And the cut is soft, on both sides. A lift that simply took everything
    // the mask touched would leave nothing here and pass the complement above.
    let edge = (source.height / 2 * source.width + (source.width - 1)) as usize;
    assert!(
        (1..255).contains(&moved[edge]),
        "the moved edge lost the selection's antialiasing, got {}",
        moved[edge]
    );
    assert!(
        (1..255).contains(&kept[edge]),
        "the hole's edge lost the selection's antialiasing, got {}",
        kept[edge]
    );
}

/// A paste puts pixels down over a layer without disturbing it, which is what
/// makes an abandoned paste cost nothing. Its damage is the destination alone:
/// there is no hole to restore, so a patch spanning back to where the pixels
/// were first placed would write over work the paste never touched.
#[test]
fn a_pasted_float_leaves_the_layer_beneath_it_alone() {
    let mut h = harness_or_skip!();
    let under = PixelRect {
        x: 0,
        y: 0,
        width: 40,
        height: 40,
    };
    let blue = [30, 60, 220, 255];
    let green = [40, 200, 60, 255];
    h.write_block(0, under, blue);

    let landing = PixelRect {
        x: 20,
        y: 20,
        width: 8,
        height: 8,
    };
    let pixels: Vec<u8> = green
        .iter()
        .copied()
        .cycle()
        .take((landing.area() * 4) as usize)
        .collect();
    let preview = h
        .canvas
        .begin_float(
            &h.gpu.device,
            &h.gpu.queue,
            1,
            &FloatSource {
                slot: 0,
                rect: landing,
                pixels: Some(&pixels),
                mask: None,
            },
        )
        .expect("no room for a preview");

    let xf = Transform::identity(landing);
    let params = FloatParams {
        inverse: xf.inverse(),
        dest: xf.dest_rect(UVec2::splat(DOC)),
    };
    let mut enc = h.encoder();
    h.canvas.draw_float(&h.gpu.queue, &mut enc, &params);
    h.gpu.queue.submit(Some(enc.finish()));

    assert_eq!(h.pixel_in(preview, 24, 24), green, "the paste is not there");
    assert_eq!(h.pixel_in(preview, 5, 5), blue, "the layer under it moved");
    assert_eq!(h.pixel(24, 24), blue, "the layer was written to early");

    let damage = xf
        .damage(UVec2::splat(DOC), false)
        .expect("something to do");
    let mut enc = h.encoder();
    h.canvas
        .commit_float(&h.gpu.queue, &mut enc, damage, &params);
    h.gpu.queue.submit(Some(enc.finish()));
    h.canvas.end_float(&h.gpu.queue);
    assert_eq!(h.pixel(24, 24), green, "the paste did not commit");
    assert_eq!(h.pixel(5, 5), blue, "the commit reached past its damage");
}

/// The drag has to clean up after itself. A preview that only drew where the
/// pixels are now would leave every earlier position of them on the canvas —
/// which is why `draw_float` restores the span of the previous destination and
/// this one rather than only its own.
#[test]
fn a_dragged_float_leaves_no_trail_behind_it() {
    let mut h = harness_or_skip!();
    let block = PixelRect {
        x: 4,
        y: 4,
        width: 8,
        height: 8,
    };
    let red = [220, 40, 30, 255];
    h.write_block(0, block, red);

    let mut xf = Transform::identity(block);
    let preview = h
        .canvas
        .begin_float(
            &h.gpu.device,
            &h.gpu.queue,
            1,
            &FloatSource {
                slot: 0,
                rect: block,
                pixels: None,
                mask: None,
            },
        )
        .expect("no room for a preview");

    for step in 1..=3 {
        xf.offset = Vec2::splat(10.0 * step as f32);
        let params = FloatParams {
            inverse: xf.inverse(),
            dest: xf.dest_rect(UVec2::splat(DOC)),
        };
        let mut enc = h.encoder();
        h.canvas.draw_float(&h.gpu.queue, &mut enc, &params);
        h.gpu.queue.submit(Some(enc.finish()));
    }

    assert_eq!(h.pixel_in(preview, 36, 36), red, "the block is not here");
    for at in [(6, 6), (16, 16), (26, 26)] {
        assert_eq!(
            h.pixel_in(preview, at.0, at.1),
            [0, 0, 0, 0],
            "a copy was left at {at:?}"
        );
    }
}

// --- patches cut to the cells a stroke reached -----------------------------

/// A canvas large enough to hold several damage cells, with a layer full of
/// pixels no two of which are alike.
///
/// Random, because the point of every test below is that certain bytes come
/// back *exactly*: a flat layer would pass them all while restoring nothing.
fn noisy_canvas(gpu: &Gpu, side: u32) -> CanvasRenderer {
    let mut canvas = CanvasRenderer::new(
        &gpu.device,
        &gpu.queue,
        UVec2::splat(side),
        TARGET_FORMAT,
        1,
    );
    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    canvas.clear_all_layers(&gpu.queue);
    canvas.clear_stroke(&gpu.device, &mut enc);
    gpu.queue.submit(Some(enc.finish()));

    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let pixels: Vec<u8> = (0..(side as usize * side as usize * 4))
        .map(|i| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            // Alpha always full, so every pixel is a valid premultiplied one
            // whatever the noise did to the colour.
            if i % 4 == 3 { 255 } else { (seed >> 24) as u8 }
        })
        .collect();
    canvas.write_layer_rect(&gpu.device, &gpu.queue, 0, whole_of(side), &pixels);
    canvas
}

/// Read a layer thumbnail through the real two-pass, non-blocking path.
///
/// The loop is what the frame loop does: record whatever pass is due, submit,
/// map, collect. Bounded because a job that never answers is a hang rather than
/// a failure, which is the worst way for CI to break. A free function rather
/// than only a `Harness` method because the wide-canvas test below drives a
/// renderer of its own, and two copies of this loop is two things to keep in
/// step about which pass is due when.
fn thumbnail_of(gpu: &Gpu, canvas: &mut CanvasRenderer, slot: u32) -> Thumbnail {
    assert!(canvas.begin_thumb(slot), "a thumbnail was in flight");
    for _ in 0..64 {
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        canvas.drive_thumb(&gpu.device, &mut enc);
        gpu.queue.submit(Some(enc.finish()));
        canvas.submit_thumb();
        let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
        if let Some(thumb) = canvas.take_thumb(&gpu.device) {
            return thumb;
        }
    }
    panic!("the thumbnail never came home");
}

fn whole_of(side: u32) -> PixelRect {
    PixelRect {
        x: 0,
        y: 0,
        width: side,
        height: side,
    }
}

/// One diagonal stroke's dabs, the damage they mark and the box they span.
fn diagonal_stroke(side: u32) -> (Vec<Dab>, TileMask, PixelRect) {
    let mut mask = TileMask::default();
    let mut bounds = Rect::empty();
    let dabs: Vec<Dab> = (0..side / 4)
        .map(|i| {
            let p = i as f32 * 4.0 + 2.0;
            let r = 5.0;
            mask.mark(Vec2::new(p, p), Vec2::splat(r));
            bounds.union_box(Vec2::new(p, p), Vec2::splat(r));
            dab(p, p, r, 1.0)
        })
        .collect();
    let rect = bounds
        .to_pixels_clamped(UVec2::splat(side))
        .expect("the stroke is on the canvas");
    (dabs, mask, rect)
}

/// The guarantee the whole tiled-patch scheme rests on: **an undo restores
/// every pixel the stroke changed, and changes nothing else.**
///
/// A patch no longer holds the stroke's bounding box, so this is no longer made
/// true by the size of the box. It is true because the commit is scissored to
/// the same pieces the patch was captured from — and if those two ever
/// disagree, either a pixel is committed with no record of what it replaced or
/// the mask misses a cell a dab reached. Both show up here as a layer that does
/// not come back, and nothing cheaper than reading the whole of it twice
/// catches either.
#[test]
fn an_undo_restores_every_pixel_a_tiled_stroke_changed() {
    let Some(gpu) = shared_gpu() else {
        eprintln!("no adapter; skipping");
        return;
    };
    static SERIAL: Mutex<()> = Mutex::new(());
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    const SIDE: u32 = 512;
    let mut canvas = noisy_canvas(gpu, SIDE);
    let before = canvas.read_layer_rect(&gpu.device, &gpu.queue, 0, whole_of(SIDE));

    let (dabs, mask, rect) = diagonal_stroke(SIDE);
    let pieces = mask.pieces(rect);
    // The stroke has to be one tiles actually help with, or this would pass on
    // a patch that was still a bounding box.
    let kept: u64 = pieces.iter().map(PixelRect::area).sum();
    assert!(
        kept * 2 < rect.area(),
        "the pieces are {kept} of {} — not a tiled patch at all",
        rect.area()
    );

    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    canvas.begin_frame();
    canvas.draw_dabs(
        &gpu.device,
        &gpu.queue,
        &mut enc,
        &dabs,
        DabStyle::default(),
    );
    gpu.queue.submit(Some(enc.finish()));

    // Captured before the commit, exactly as `finish_stroke` does it.
    let patch = canvas.read_layer_pieces(&gpu.device, &gpu.queue, 0, &pieces);

    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    canvas.commit_stroke(
        &gpu.device,
        &gpu.queue,
        &mut enc,
        0,
        rect,
        &pieces,
        StrokeStyle {
            color: Color::from_srgb_u8(200, 20, 20, 255),
            opacity: 1.0,
            mode: BrushMode::Paint,
            blend: BlendMode::Normal,
            per_dab_color: false,
            on_mask: false,
        },
    );
    gpu.queue.submit(Some(enc.finish()));

    let painted = canvas.read_layer_rect(&gpu.device, &gpu.queue, 0, whole_of(SIDE));
    assert_ne!(painted, before, "the stroke painted nothing");

    for (piece, bytes) in pieces.iter().zip(&patch) {
        canvas.write_layer_rect(&gpu.device, &gpu.queue, 0, *piece, bytes);
    }
    let restored = canvas.read_layer_rect(&gpu.device, &gpu.queue, 0, whole_of(SIDE));

    // Byte for byte, over the whole layer — not only inside the stroke.
    if restored != before {
        let differing = restored.iter().zip(&before).filter(|(a, b)| a != b).count();
        panic!(
            "the undo left {differing} bytes of {} changed",
            before.len()
        );
    }
}

/// Reading the pieces together must give exactly what reading them one at a
/// time gives, whatever the device's buffer limit does to the batching.
///
/// The batched path is the one every stroke takes, and the failure it is prone
/// to is a piece landing at the wrong offset in the shared staging buffer —
/// which looks like an undo pasting one part of the canvas over another, on
/// large documents only.
#[test]
fn pieces_read_together_match_pieces_read_one_at_a_time() {
    let Some(gpu) = shared_gpu() else {
        eprintln!("no adapter; skipping");
        return;
    };
    static SERIAL: Mutex<()> = Mutex::new(());
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    const SIDE: u32 = 512;
    let mut canvas = noisy_canvas(gpu, SIDE);
    let (_, mask, rect) = diagonal_stroke(SIDE);
    let pieces = mask.pieces(rect);
    assert!(pieces.len() > 4, "the fixture is not testing batching");

    let one_at_a_time: Vec<Vec<u8>> = pieces
        .iter()
        .map(|p| canvas.read_layer_rect(&gpu.device, &gpu.queue, 0, *p))
        .collect();

    assert_eq!(
        canvas.read_layer_pieces(&gpu.device, &gpu.queue, 0, &pieces),
        one_at_a_time,
        "one submission for all the pieces read something else"
    );

    // And again with a limit small enough that the batch has to be broken up,
    // and then small enough that a single piece will not fit and falls through
    // to the banded reader. Both are what a large canvas does on real hardware,
    // and neither is reachable on a document a test can afford.
    for limit in [40 * 1024, 8 * 1024] {
        canvas.set_readback_limit(limit);
        assert_eq!(
            canvas.read_layer_pieces(&gpu.device, &gpu.queue, 0, &pieces),
            one_at_a_time,
            "a readback limit of {limit} sheared the pieces"
        );
    }
}

// --- flipping the canvas ---------------------------------------------------

/// The one thing `flip_layers` has to be: **an exact permutation of texels**.
///
/// The history entry a canvas flip records stores no pixels at all — undoing it
/// is flipping again — so any loss here would not be a one-off rounding error
/// but a drift compounding every time somebody flipped and undid. That is why
/// the pass reads with `textureLoad` through non-sRGB views and blends nothing.
/// A decode to linear and a re-encode would very nearly pass a test that only
/// looked at the mirror, which is why the second half of this reads the *whole*
/// layer back and demands the bytes it started with.
///
/// Noise rather than a flat fill for the reason `noisy_canvas` gives, and every
/// layer of a two-layer stack, because the command mirrors the picture rather
/// than the layer in front.
#[test]
fn a_flip_mirrors_the_canvas_and_flipping_twice_restores_it_exactly() {
    let Some(gpu) = shared_gpu() else {
        eprintln!("no adapter; skipping");
        return;
    };
    static SERIAL: Mutex<()> = Mutex::new(());
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    const SIDE: u32 = 64;
    let mut canvas = noisy_canvas(gpu, SIDE);
    canvas.ensure_slots(&gpu.device, &gpu.queue, 2);
    // A second layer, distinct from the first, so a flip that quietly mirrored
    // only slot 0 — or mirrored one slice into another — cannot pass.
    let second: Vec<u8> = (0..(SIDE as usize * SIDE as usize * 4))
        .map(|i| if i % 4 == 3 { 255 } else { (i * 7) as u8 })
        .collect();
    canvas.write_layer_rect(&gpu.device, &gpu.queue, 1, whole_of(SIDE), &second);

    let before: Vec<Vec<u8>> = (0..2)
        .map(|slot| canvas.read_layer_rect(&gpu.device, &gpu.queue, slot, whole_of(SIDE)))
        .collect();

    for axis in [FlipAxis::Horizontal, FlipAxis::Vertical] {
        canvas.flip_layers(&gpu.device, &gpu.queue, &[0, 1], axis);

        for slot in 0..2 {
            let after = canvas.read_layer_rect(&gpu.device, &gpu.queue, slot, whole_of(SIDE));
            assert_eq!(
                after,
                mirrored(&before[slot as usize], SIDE, axis),
                "slot {slot} is not the mirror of what it was, about {axis:?}"
            );
        }

        // And back. This is the assertion the design rests on: not "close
        // enough", but the same bytes.
        canvas.flip_layers(&gpu.device, &gpu.queue, &[0, 1], axis);
        for slot in 0..2 {
            assert_eq!(
                canvas.read_layer_rect(&gpu.device, &gpu.queue, slot, whole_of(SIDE)),
                before[slot as usize],
                "flipping slot {slot} twice about {axis:?} moved the picture"
            );
        }
    }
}

// --- layer thumbnails -------------------------------------------------------

/// The whole point of the two passes: a mark occupying an eighth of the canvas
/// comes back **filling** the thumbnail rather than as a speck in the corner of
/// a shrunken canvas.
///
/// Also pins the empty answer, which is a real answer and not a failure —
/// without it the layer list would ask again on every frame for as long as a
/// blank layer was in the stack.
#[test]
fn a_thumbnail_shows_the_layers_content_and_not_the_whole_canvas() {
    let Some(mut h) = Harness::new() else { return };
    let side = umber_core::thumbnail::SIZE as usize;
    let alpha = |px: &[u8], x: usize, y: usize| px[(y * side + x) * 4 + 3];

    // A canvas big enough that scaling the content up is the interesting case.
    // On the harness's own 64² the "never magnify" cap binds for any mark, and
    // this test would then be asserting that rule rather than this one — see
    // `a_tiny_mark_is_never_magnified` in `umber_core::thumbnail`.
    const SIDE: u32 = 512;
    h.canvas.resize(
        &h.gpu.device,
        &h.gpu.queue,
        UVec2::splat(SIDE),
        Anchor::TopLeft,
        1,
    );

    assert!(
        h.thumbnail(0).is_empty(),
        "a layer nobody has painted on has nothing to show"
    );

    // An eighth of the canvas, hard against the top-left corner — the case a
    // whole-canvas thumbnail renders as four grey pixels.
    h.write_block(
        0,
        PixelRect {
            x: 0,
            y: 0,
            width: SIDE / 8,
            height: SIDE / 8,
        },
        [255, 0, 0, 255],
    );
    let thumb = h.thumbnail(0);
    assert!(!thumb.is_empty());
    assert_eq!(thumb.slot, 0);

    let px = &thumb.rgba;
    assert_eq!(
        alpha(px, side / 2, side / 2),
        255,
        "the mark should be in the middle of the frame"
    );
    // The mark is square and so is the frame, so it fills all but the padding:
    // a few texels in from the edge is still ink.
    let inset = (side as f32 * (umber_core::thumbnail::PADDING + 0.04)) as usize;
    assert_eq!(
        alpha(px, inset, inset),
        255,
        "the mark should fill the frame"
    );
    assert_eq!(alpha(px, 0, 0), 0, "and there should be a margin around it");
    // Red went in, red comes out: the pass un-premultiplies and sRGB-encodes,
    // which is the form `Color32::from_rgba_unmultiplied` is handed.
    let mid = (side / 2 * side + side / 2) * 4;
    assert_eq!(&px[mid..mid + 3], &[255, 0, 0]);
}

/// **A one-pixel column on a very wide canvas is still found**, which is the
/// behavioural half of `the_thumbnail_pass_never_steps_over_a_texel_on_any_
/// canvas_umber_admits` in `canvas.rs`.
///
/// The bounds pass reduces by maximum precisely so that a sketch survives being
/// shrunk into a 64-square, and `MAX_TAPS` used to undo that on a wide canvas:
/// past a span of 256 source texels per destination texel the loop stepped, so
/// it visited every other column and a thin vertical mark could fall between
/// the taps entirely. `content_rect` then answered `None` and the row drew the
/// same checker a blank layer draws, on a layer somebody had painted on. That
/// is not cosmetic on its own terms and it is about to matter more: a scheme
/// that takes a thumbnail before evicting a layer from VRAM would cache the
/// wrong answer permanently, because the cache is keyed on a revision that has
/// stopped moving.
///
/// **Two adjacent columns, because one alone proves nothing.** Within a
/// destination texel a step of two visits `first`, `first + 2`, …, so of any two
/// neighbouring columns exactly one is visited and exactly one is skipped —
/// which parity depends on the canvas is not worth reimplementing here, and
/// asking about both makes the case deterministic without doing so.
///
/// **What this does not cover.** It is only a real question on an adapter that
/// will make a canvas past 16384, since at exactly 16384 the bounds pass's span
/// is exactly 256 and nothing stepped. That is a Vulkan device with a large
/// card — an RTX 3080 reports 32768 — and it is *not* WARP, lavapipe, or any
/// D3D12 or Metal device, all of which cap at 16384. So on CI this passes
/// without exercising the case, and says so rather than looking like cover it
/// does not give. The picture pass's own worse bound is what a device capped at
/// 16384 could reach, and it is pinned by arithmetic rather than by ink.
#[test]
fn a_thin_mark_on_the_widest_canvas_this_device_admits_is_still_found() {
    let h = harness_or_skip!();

    let width = h
        .gpu
        .device
        .limits()
        .max_texture_dimension_2d
        .min(umber_core::Document::MAX_EDGE);
    // Short, so the slice is a few megabytes rather than a few gigabytes: what
    // is under test is the span along one axis, and the width is that axis.
    const HEIGHT: u32 = 64;

    let mut canvas =
        h.canvas
            .for_document(&h.gpu.device, &h.gpu.queue, UVec2::new(width, HEIGHT), 1);
    let mut enc = h.encoder();
    canvas.clear_all_layers(&h.gpu.queue);
    canvas.clear_stroke(&h.gpu.device, &mut enc);
    h.gpu.queue.submit(Some(enc.finish()));

    let column: Vec<u8> = [255u8, 0, 0, 255].repeat(HEIGHT as usize);
    for x in [width / 2, width / 2 + 1] {
        // A fresh slice each time, so the second reading cannot be the first
        // one's ink still standing.
        let enc = h.encoder();
        canvas.clear_all_layers(&h.gpu.queue);
        h.gpu.queue.submit(Some(enc.finish()));

        canvas.write_layer_rect(
            &h.gpu.device,
            &h.gpu.queue,
            0,
            PixelRect {
                x,
                y: 0,
                width: 1,
                height: HEIGHT,
            },
            &column,
        );

        let thumb = thumbnail_of(h.gpu, &mut canvas, 0);
        assert!(
            !thumb.is_empty(),
            "a one-pixel column at x = {x} on a {width}-wide canvas was reported \
             as an empty layer",
        );
        assert!(
            thumb.rgba.iter().skip(3).step_by(4).any(|a| *a > 0),
            "the layer was found but its picture holds no ink",
        );
    }
}

/// A layer written to **between** a thumbnail's two passes must not wedge the
/// job.
///
/// This was a real bug. The bounds pass leaves the job waiting at the end of one
/// frame and the picture pass is recorded on the next, so a stroke committing —
/// which is to say every pointer-up — lands in that gap and disowns it. With the
/// abandoned check folded into the mapped arm alone, the job could never be
/// collected, never be re-driven and never be dropped: `thumb_in_flight` stayed
/// true for the life of the renderer, so no layer thumbnail ever updated again
/// *and* `app.rs` requested a redraw every frame for ever.
#[test]
fn a_layer_written_between_a_thumbnails_two_passes_does_not_wedge_it() {
    let Some(mut h) = Harness::new() else { return };
    let block = PixelRect {
        x: 4,
        y: 4,
        width: 8,
        height: 8,
    };
    h.write_block(0, block, [255, 0, 0, 255]);

    assert!(h.canvas.begin_thumb(0));
    // Drive until the bounds pass has come home — the job is then waiting, with
    // its region decided and its picture pass not yet recorded.
    for _ in 0..8 {
        let mut enc = h.encoder();
        h.canvas.drive_thumb(&h.gpu.device, &mut enc);
        h.gpu.queue.submit(Some(enc.finish()));
        h.canvas.submit_thumb();
        let _ = h.gpu.device.poll(wgpu::PollType::wait_indefinitely());
        assert!(
            h.canvas.take_thumb(&h.gpu.device).is_none(),
            "the picture cannot have arrived before the second pass ran"
        );
        if h.canvas.thumb_phase_is_picture() {
            break;
        }
    }
    assert!(
        h.canvas.thumb_phase_is_picture(),
        "the bounds pass never came home"
    );

    // Paint on the layer, which is what disowns the job.
    h.write_block(0, block, [0, 255, 0, 255]);

    // One collection is all it should take to give the job back.
    assert!(h.canvas.take_thumb(&h.gpu.device).is_none());
    assert!(
        !h.canvas.thumb_in_flight(),
        "the disowned job was never dropped — the list is frozen and the loop \
         will never sleep again"
    );
    assert!(h.canvas.begin_thumb(0), "and a fresh one can be asked for");
}

/// The invalidation rule, at the level that owns it. Every route that writes a
/// slice moves that slice's counter and no other — which is what lets the layer
/// list cache a picture and know exactly when it has stopped being true.
#[test]
fn writing_a_slice_moves_its_revision_and_leaves_the_others_alone() {
    let Some(mut h) = Harness::new() else { return };
    let before = [h.canvas.slot_revision(0), h.canvas.slot_revision(1)];

    h.stamp(&[dab(32.0, 32.0, 8.0, 1.0)]);
    assert_eq!(
        [h.canvas.slot_revision(0), h.canvas.slot_revision(1)],
        before,
        "a stroke that has not been committed has changed no layer"
    );

    h.commit_to(0, Color::BLACK, 1.0, BrushMode::Paint);
    assert!(h.canvas.slot_revision(0) > before[0], "the commit wrote it");
    assert_eq!(
        h.canvas.slot_revision(1),
        before[1],
        "and wrote nothing else"
    );

    // The other two routes into a layer's pixels, each of which the layer list
    // depends on being counted.
    let after_commit = h.canvas.slot_revision(0);
    h.write_block(
        0,
        PixelRect {
            x: 0,
            y: 0,
            width: 4,
            height: 4,
        },
        [1, 2, 3, 4],
    );
    assert!(
        h.canvas.slot_revision(0) > after_commit,
        "an undo writes its patch back through `write_layer_rect`"
    );

    let after_write = h.canvas.slot_revision(0);
    let enc = h.encoder();
    h.canvas.clear_layer(&h.gpu.queue, 0);
    h.gpu.queue.submit(Some(enc.finish()));
    assert!(h.canvas.slot_revision(0) > after_write);
}

/// The mirror of a tightly packed square of RGBA8, done on the CPU.
fn mirrored(px: &[u8], side: u32, axis: FlipAxis) -> Vec<u8> {
    let mut out = vec![0u8; px.len()];
    for y in 0..side {
        for x in 0..side {
            let (sx, sy) = match axis {
                FlipAxis::Horizontal => (side - 1 - x, y),
                FlipAxis::Vertical => (x, side - 1 - y),
            };
            let d = ((y * side + x) * 4) as usize;
            let s = ((sy * side + sx) * 4) as usize;
            out[d..d + 4].copy_from_slice(&px[s..s + 4]);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Layer effects
// ---------------------------------------------------------------------------
//
// `docs/layer-effects.md` §11's GPU list. Three of these are the gates the
// design names, and each is invisible to a test built out of the other two: the
// identity says the fast path is really a fast path, the Multiply says an
// effect composites against the *backdrop* rather than against its own layer,
// and the knockout says an outer effect does not paint under the shape it came
// from.
//
// Every assertion about a soft edge is on **alpha**, read out of the effect
// slice, with a level of slack — CLAUDE.md's rule for a value that has been
// through a shader. Where the arithmetic has nothing to round, an exact
// comparison is used and said to be exact.

/// The square every effect below is derived from: 24..40 on both axes, so it is
/// centred on the canvas and its corner is 24 texels in from each edge.
const SHAPE: PixelRect = PixelRect {
    x: 24,
    y: 24,
    width: 16,
    height: 16,
};

/// The whole canvas, for a floor to composite an effect against.
const WHOLE: PixelRect = PixelRect {
    x: 0,
    y: 0,
    width: DOC,
    height: DOC,
};

impl Harness {
    /// Bake, with nothing in flight.
    fn bake(&mut self, stack: &[LayerEffects<'_>], base: u32) -> BakedStack {
        self.bake_frame(
            stack,
            base,
            EffectFrame {
                active_index: u32::MAX,
                stroke: StrokeStyle {
                    opacity: 0.0,
                    ..Default::default()
                },
                stroke_live: false,
            },
        )
    }

    fn bake_frame(
        &mut self,
        stack: &[LayerEffects<'_>],
        base: u32,
        frame: EffectFrame,
    ) -> BakedStack {
        let mut enc = self.encoder();
        let baked = self.canvas.bake_effects(
            &self.gpu.device,
            &self.gpu.queue,
            &mut enc,
            base,
            stack,
            frame,
        );
        self.gpu.queue.submit(Some(enc.finish()));
        baked
    }
}

/// One layer's worth of the bake's input.
fn effected<'a>(draw: LayerDraw, effects: &'a [Effect]) -> LayerEffects<'a> {
    LayerEffects { draw, effects }
}

/// The alpha of one texel of a slice.
fn slice_alpha(h: &Harness, slot: u32, x: u32, y: u32) -> u8 {
    h.canvas.read_layer_rect(
        &h.gpu.device,
        &h.gpu.queue,
        slot,
        PixelRect {
            x,
            y,
            width: 1,
            height: 1,
        },
    )[3]
}

/// A drop shadow at `angle`/`distance`, otherwise inert: no spread, no
/// softness, opaque, Normal.
fn shadow(color: Color, angle: f32, distance: f32) -> Effect {
    Effect {
        color,
        opacity: 1.0,
        blend: BlendMode::Normal,
        spread: 0.0,
        softness: 0.0,
        angle,
        distance,
        ..Effect::drop_shadow()
    }
}

/// **The extract must read a mask the way the composite does.**
///
/// An effect is derived from the layer's alpha *after* its mask, so the extract
/// takes a mask tap of its own — a second reader of the same slice, in a second
/// shader. If the two disagree about what a stored byte means, a shadow is
/// derived from a coverage the picture never had, and the symptom is a shadow
/// that is merely the wrong strength: plausible, and attributable to any of half
/// a dozen parameters.
///
/// The mask is a **partial**, and that is the whole of what makes this a test.
/// Under the raw reading 128 is a multiplier of 0.502; under the sRGB reading
/// the same byte is 0.216, so the shadow comes back at 55 where it should be
/// 128. At 0 or 255 the two readings agree exactly and this sees nothing —
/// the same reason every mask fixture in this change carries a partial.
///
/// The shadow is inert apart from its distance: no spread, no softness, so the
/// slice holds the coverage itself rather than something a kernel has been over.
#[test]
fn an_effect_reads_a_mask_the_way_the_composite_does() {
    let mut h = harness_or_skip!();
    h.canvas.ensure_slots(&h.gpu.device, &h.gpu.queue, 2);
    h.write_block(0, SHAPE, [255, 255, 255, 255]);
    // A mask over the whole canvas, so the shadow's own displaced position is
    // masked by the same value the shape is and there is one number in play.
    h.write_block(1, WHOLE, [128, 128, 128, 255]);
    h.canvas.mark_mask_slot(1);

    let mut draw = layer(0, 1.0, BlendMode::Normal);
    draw.mask = Some(1);
    // Angle 180 puts the offset at +x, the convention
    // `a_drop_shadow_at_multiply_multiplies_against_the_backdrop` already
    // relies on, and 20 clears `SHAPE`'s 16-wide square entirely — so the point
    // read below is solid shadow with none of the layer over it and none of the
    // knockout taken out of it.
    let effects = [shadow(Color::new(1.0, 0.0, 0.0, 1.0), 180.0, 20.0)];
    let baked = h.bake(&[effected(draw, &effects)], 2);
    let slot = baked.draws[0].slot;

    // The middle of the shape displaced by 20 in x.
    let a = slice_alpha(&h, slot, 52, 32);
    assert!(
        a.abs_diff(128) <= 3,
        "the shadow of a half-masked layer came back at {a}; 128 is the mask \
         read as the composite reads it and 55 is the sRGB reading this replaced"
    );
}

/// An outline of `spread`, hard-edged, at `position`.
fn outline(color: Color, spread: f32, position: OutlinePosition) -> Effect {
    Effect {
        color,
        opacity: 1.0,
        spread,
        softness: 0.0,
        position,
        ..Effect::outline()
    }
}

/// **The first gate.** An outline of no width and a shadow of no size are the
/// exact identity: not "nearly the same picture", but no draw at all and
/// therefore the same bytes.
///
/// It is what says the fast path is really a fast path — the rule the selection's
/// feather, the brush's grain and the selection clip all keep. The shadow half is
/// a *decision* rather than arithmetic, and `effect_marks_nothing` is where it is
/// argued: such a shadow is its own shape directly under its own shape, so the
/// knockout leaves only a rim at the antialiased edge, at `c(1 - c)`. Photoshop
/// draws that rim; Umber declines to, because declining is what makes this test
/// an identity.
#[test]
fn an_effect_with_no_reach_produces_no_draw_at_all() {
    let mut h = harness_or_skip!();
    h.write_block(0, SHAPE, [255, 255, 255, 255]);

    let plain = [layer(0, 1.0, BlendMode::Normal)];
    let bare = h.composite_pixel(&plain, 32, 32);
    let edge = h.composite_pixel(&plain, 24, 24);

    let inert = [
        outline(Color::BLACK, 0.0, OutlinePosition::Outside),
        shadow(Color::BLACK, 120.0, 0.0),
    ];
    let stack = [effected(plain[0], &inert)];
    let baked = h.bake(&stack, 1);

    assert_eq!(
        baked.draws.len(),
        1,
        "an effect with no reach still produced a draw"
    );
    assert_eq!(baked.dropped, 0);
    assert_eq!(
        h.canvas.effect_bakes(),
        0,
        "nothing to draw and a pass was still recorded"
    );
    assert_eq!(h.composite_pixel(&baked.draws, 32, 32), bare);
    assert_eq!(
        h.composite_pixel(&baked.draws, 24, 24),
        edge,
        "the antialiased edge moved, which is the rim this rule refuses"
    );
}

/// An effect on a layer the composite discards is not baked, not given a slice
/// and not drawn — and taking it out moves no pixel.
///
/// `composite.wgsl`'s loop reads a hidden layer's texels and then `continue`s
/// past `acc`, so an effect derived from one is a canvas-sized slice and up to
/// several full-screen passes a frame for a picture nobody sees. The predicate
/// here is that shader's own, `!visible || opacity <= 0.0`.
///
/// **Five outputs, and the last two are the ones that matter.** The bake count,
/// the slot capacity and the draw list say the work did not happen; the
/// composite says the elision was safe; and `active_index` says the draw list is
/// still describable, since eliding a draw shifts every position after it and
/// the stroke previews on whichever draw sits at the number the composite is
/// given. The clipped layer above the hidden one is there deliberately: an
/// unclipped invisible draw is the one shape `layer-residency.md` §2.2 warns
/// against removing, because it writes `clip_alpha` for whatever is clipped to
/// it — an effect draw cannot be that draw, because its own layer's draw follows
/// immediately and writes the same zero, and this is what says so out loud.
///
/// **Both halves of the predicate are driven**, hidden and at zero opacity: they
/// are `composite.wgsl`'s own `!visible || opacity <= 0.0` and a test of only the
/// first would leave the second free to be deleted.
///
/// And showing the layer again brings the effect back, so this is an elision
/// rather than a loss.
///
/// `base` is **4 and not 3**, which is the harness's whole capacity: an effect
/// slice at 3 would fit in the array already and the capacity reading would be
/// the same number either way, which is a guard agreeing with itself. At 4 the
/// un-elided path has to grow. It also keeps the ghost's slot 3 clear of
/// anything a bake writes.
#[test]
fn an_effect_on_a_layer_that_is_not_composited_is_never_baked() {
    let mut h = harness_or_skip!();
    h.write_block(2, WHOLE, [128, 128, 128, 255]);
    h.write_block(0, SHAPE, [255, 255, 255, 255]);
    h.write_block(1, WHOLE, [255, 0, 0, 255]);

    let floor = layer(2, 1.0, BlendMode::Normal);
    let hidden = LayerDraw {
        visible: false,
        ..layer(0, 1.0, BlendMode::Normal)
    };
    // Clipped to the layer below it, which is the hidden one. Its own draw is
    // what has to keep bounding this to nothing once the effect draws are gone.
    let clipped = LayerDraw {
        clipped: true,
        ..layer(1, 1.0, BlendMode::Normal)
    };

    // Angle 180 puts the offset at +x, clear of the square, exactly as
    // `a_drop_shadow_at_multiply_multiplies_against_the_backdrop` does.
    let cast = [shadow(Color::BLACK, 180.0, 12.0)];
    let plain = [floor, hidden, clipped];
    let stack = [
        effected(floor, &[]),
        effected(hidden, &cast),
        effected(clipped, &[]),
    ];

    let before_bakes = h.canvas.effect_bakes();
    let before_slots = h.canvas.page_count();
    // The stroke is on the hidden layer, whose own outer effect is the draw
    // being elided — the position that shifts if `baked` counts wrongly.
    let painting = EffectFrame {
        active_index: 1,
        stroke: StrokeStyle {
            opacity: 0.0,
            ..Default::default()
        },
        stroke_live: false,
    };
    let baked = h.bake_frame(&stack, 4, painting);

    // The capacity first, deliberately: it is the reading a shorter test would
    // never reach, because the draw count below fails on the same mutation and
    // would mask it.
    assert_eq!(
        h.canvas.page_count(),
        before_slots,
        "a hidden layer's effect took a canvas-sized page"
    );
    assert_eq!(
        baked.draws.len(),
        3,
        "a hidden layer's effect still produced a draw: {:?}",
        baked.draws
    );
    assert_eq!(
        h.canvas.effect_bakes(),
        before_bakes,
        "a hidden layer's effect was baked into a slice nothing reads"
    );
    assert_eq!(
        baked.active_index, 1,
        "the stroke would preview on the wrong draw"
    );
    assert_eq!(baked.dropped, 0);

    // The other half of the predicate: visible, and at zero opacity.
    let faded = LayerDraw {
        opacity: 0.0,
        ..layer(0, 1.0, BlendMode::Normal)
    };
    let baked_faded = h.bake_frame(
        &[
            effected(floor, &[]),
            effected(faded, &cast),
            effected(clipped, &[]),
        ],
        4,
        painting,
    );
    assert_eq!(
        baked_faded.draws.len(),
        3,
        "a layer at zero opacity still baked its effect: {:?}",
        baked_faded.draws
    );
    assert_eq!(h.canvas.effect_bakes(), before_bakes);
    assert_eq!(h.canvas.page_count(), before_slots);

    // (45, 32) is inside where the shadow would have fallen and clear of the
    // square, so it is the pixel that would move if any of this were unsound.
    let probes = [(45, 32), (32, 32), (24, 24), (0, 0)];
    for (x, y) in probes {
        assert_eq!(
            h.composite_pixel(&baked.draws, x, y),
            h.composite_pixel(&plain, x, y),
            "the picture moved at ({x}, {y}) when the effect was elided"
        );
    }

    // **And the elision itself is what has to be shown safe.** The loop above
    // compares two lists that are equal whenever the code is right, so on its
    // own it agrees with itself. This drives `composite.wgsl` directly instead:
    // an effect draw of a hidden layer, present and invisible, over a slice full
    // of ink loud enough that any leak would be obvious, against the same stack
    // with it taken out. If an invisible unclipped draw could ever reach `acc`
    // or leave `clip_alpha` somewhere the layer's own draw does not, this is
    // where it shows.
    h.write_block(3, WHOLE, [0, 255, 0, 255]);
    let ghost = LayerDraw {
        visible: false,
        ..layer(3, 1.0, BlendMode::Normal)
    };
    let with_ghost = [floor, ghost, hidden, clipped];
    for (x, y) in probes {
        assert_eq!(
            h.composite_pixel(&with_ghost, x, y),
            h.composite_pixel(&plain, x, y),
            "an invisible effect draw was not free at ({x}, {y}), so eliding \
             one is not free either"
        );
    }

    // Show it again and the effect comes back: this is an elision, not a loss.
    let shown = layer(0, 1.0, BlendMode::Normal);
    let lit = [
        effected(floor, &[]),
        effected(shown, &cast),
        effected(clipped, &[]),
    ];
    let baked = h.bake_frame(&lit, 4, painting);
    assert_eq!(
        baked.draws.len(),
        4,
        "showing the layer again did not bring its effect back"
    );
    assert!(
        h.canvas.effect_bakes() > before_bakes,
        "showing the layer again drew a shadow nothing had baked"
    );
    assert!(
        h.canvas.page_count() > before_slots,
        "the effect drew into a page the atlas never grew to hold"
    );
    // A drop shadow is an *outer* effect, so its draw is spliced in before its
    // layer's and the stroke's position moves with it. This is the reading the
    // elided case above has to agree with, and the pair is what says
    // `active_index` is counted off what was pushed rather than off the caller's
    // list.
    assert_eq!(
        baked.active_index, 2,
        "the stroke would preview on the shadow instead of the layer"
    );
}

/// A canvas too large to speculate on gives the effect working set's optional
/// planes back once nothing wants them, and the picture is unchanged.
///
/// `ensure_effect_scratch` keeps the seed pair and the band plane once they have
/// been allocated, because an effect whose spread is dragged crosses zero
/// repeatedly. Sound at 2048², where the pair is 16 MB; at 100 Mpx it is 800 MB.
///
/// **Three readings and a pixel.** The seed pair arrives with a flooding effect,
/// stays while the document still wants it, goes when the spread reaches zero,
/// and the shadow the document then draws is byte for byte the shadow a document
/// that never had a spread draws. The last is the one that matters: nothing here
/// may move a pixel.
#[test]
fn a_large_canvas_gives_the_effect_working_set_back_when_nothing_wants_it() {
    let mut h = harness_or_skip!();
    h.write_block(0, SHAPE, [255, 255, 255, 255]);
    h.canvas.set_speculation_limit(0);

    let draw = layer(0, 1.0, BlendMode::Normal);
    let probes = [(45, 32), (32, 32), (24, 24)];

    // The reference, taken before a seed pair has ever existed: a shadow with
    // no spread needs no flood.
    let flat = [shadow(Color::BLACK, 180.0, 12.0)];
    let baked = h.bake(&[effected(draw, &flat)], 1);
    assert!(
        !h.canvas.effect_working_set().1,
        "a shadow with no spread allocated a seed pair"
    );
    let reference: Vec<[u8; 4]> = probes
        .iter()
        .map(|(x, y)| h.composite_pixel(&baked.draws, *x, *y))
        .collect();

    // Give it a spread, which is what needs the flood and its seed pair.
    let spread = [Effect {
        spread: 4.0,
        ..shadow(Color::BLACK, 180.0, 12.0)
    }];
    h.bake(&[effected(draw, &spread)], 1);
    assert_eq!(
        h.canvas.effect_working_set(),
        (true, true, false),
        "a flooding effect did not allocate the seed pair"
    );

    // Still wanted, so still held: this is not a per-frame drop.
    h.bake(&[effected(draw, &spread)], 1);
    assert_eq!(
        h.canvas.effect_working_set(),
        (true, true, false),
        "the seed pair was dropped while an effect still wanted it"
    );

    // Spread back to zero: nothing wants the seeds, and on this canvas they go.
    let baked = h.bake(&[effected(draw, &flat)], 1);
    assert!(
        !h.canvas.effect_working_set().1,
        "a canvas too large to speculate on held the seed pair anyway"
    );
    for ((x, y), was) in probes.iter().zip(&reference) {
        assert_eq!(
            h.composite_pixel(&baked.draws, *x, *y),
            *was,
            "the shadow moved at ({x}, {y}) when the working set was trimmed"
        );
    }
}

/// **The second gate.** A drop shadow at Multiply multiplies against *the
/// backdrop* — what is under the layer — and not against its own layer.
///
/// This is the whole point of an effect being its own draw entry rather than
/// pixels baked into the layer's slice, and it is invisible in any test built
/// only out of Normal. Stated as a blend identity: Multiply by white is exactly
/// the identity, so the pixel under the shadow must come back as the grey layer's
/// own colour, byte for byte. The Normal reading is asserted beside it to show
/// the test can tell the two apart at all.
#[test]
fn a_drop_shadow_at_multiply_multiplies_against_the_backdrop() {
    let mut h = harness_or_skip!();
    // A mid-grey floor over the whole canvas, and an opaque square above it.
    h.write_block(1, WHOLE, [128, 128, 128, 255]);
    h.write_block(0, SHAPE, [255, 255, 255, 255]);

    let floor = layer(1, 1.0, BlendMode::Normal);
    let top = layer(0, 1.0, BlendMode::Normal);
    // Angle 180 puts the offset squarely at +x: the shadow's shape is 36..52,
    // so (45, 32) is inside the shadow and clear of the square.
    let (x, y) = (45, 32);
    let floor_only = h.composite_pixel(&[floor], x, y);

    let white = [Effect {
        blend: BlendMode::Multiply,
        ..shadow(Color::WHITE, 180.0, 12.0)
    }];
    let stack = [effected(floor, &[]), effected(top, &white)];
    let baked = h.bake(&stack, 2);
    assert_eq!(baked.draws.len(), 3, "{:?}", baked.draws);
    assert_eq!(
        h.composite_pixel(&baked.draws, x, y),
        floor_only,
        "Multiply by white is the identity, so the backdrop must come back \
         unchanged — it did not, so the shadow is not multiplying against it"
    );

    let normal = [shadow(Color::WHITE, 180.0, 12.0)];
    let stack = [effected(floor, &[]), effected(top, &normal)];
    let baked = h.bake(&stack, 2);
    let over = h.composite_pixel(&baked.draws, x, y);
    assert_ne!(
        over, floor_only,
        "the same shadow at Normal must replace the backdrop, or this test \
         cannot tell the two modes apart"
    );
    assert_near(over, [255, 255, 255], 2, "a white shadow at Normal");
}

/// **The third gate.** A layer at 50% opacity over a shadow shows no shadow
/// inside its own shape.
///
/// The knockout is §3.3's, baked rather than composited: the bake has the
/// coverage in hand already, so multiplying by `1 - coverage` costs nothing
/// there, where doing it at composite time would need an *inverse* clip the
/// shader has no notion of. Fifty per cent is what makes the test bite — at full
/// opacity the layer hides its own shadow whether or not anything knocked it out.
#[test]
fn a_layer_knocks_its_own_drop_shadow_out() {
    let mut h = harness_or_skip!();
    h.write_block(1, WHOLE, [0, 0, 0, 255]);
    h.write_block(0, SHAPE, [255, 255, 255, 255]);

    let floor = layer(1, 1.0, BlendMode::Normal);
    let half = layer(0, 0.5, BlendMode::Normal);
    let without = h.composite_pixel(&[floor, half], 32, 32);

    // A spread rather than a displacement, so the shadow's shape covers the
    // square rather than sitting beside it — which is the only arrangement in
    // which the knockout is what decides the answer.
    let red = [Effect {
        spread: 8.0,
        ..shadow(Color::new(1.0, 0.0, 0.0, 1.0), 120.0, 0.0)
    }];
    let stack = [effected(floor, &[]), effected(half, &red)];
    let baked = h.bake(&stack, 2);
    assert_eq!(baked.draws.len(), 3);

    assert_eq!(
        h.composite_pixel(&baked.draws, 32, 32),
        without,
        "a shadow showed through the middle of its own shape"
    );
    // And it is still there where the shape is not, or the knockout has taken
    // the whole effect rather than the part under the layer.
    let outside = h.composite_pixel(&baked.draws, 32, 18);
    assert!(
        outside[0] > outside[2] + 32,
        "the shadow is missing outside the shape: {outside:?}"
    );
}

/// The blur is mirrored about both axes.
///
/// The property two box passes per axis buy, and the one that catches an
/// off-by-one in a separable pass — a kernel that reached one texel further one
/// way than the other would leave the mark lopsided by a level, which is
/// invisible in a picture and exact here. Read off the effect slice's alpha, so
/// the reading is the bake's own output rather than something the composite has
/// blended.
///
/// The shape is centred on a texel *boundary*, so texel `i` mirrors to `63 - i`
/// with nothing to interpolate.
///
/// **Both resolutions**, and the second one is why this test is written as a
/// sweep. The blur runs on a 4x downsample above `EFFECT_FULL_RES_SOFTNESS` and
/// at full resolution below it, and nothing exercised the first — which is how a
/// mis-centred upsample survived: it displaced the whole shadow a pixel and a
/// half diagonally, on that path alone, whatever the effect's own angle. A
/// diagonal shift is exactly what a mirror test sees.
#[test]
fn a_softened_shadow_is_mirrored_about_both_axes() {
    let mut h = harness_or_skip!();
    h.write_block(0, SHAPE, [255, 255, 255, 255]);
    let draw = layer(0, 1.0, BlendMode::Normal);

    // 9 is under the threshold and 34 is over it, so one of these runs the tent
    // whole and the other runs it on a quarter-resolution copy.
    for softness in [9.0, 34.0] {
        // No displacement, so the only thing that could break the symmetry is the
        // kernel. A spread as well as a softness, so the effect draws at all.
        let soft = [Effect {
            spread: 2.0,
            softness,
            distance: 0.0,
            ..Effect::drop_shadow()
        }];
        let stack = [effected(draw, &soft)];
        let baked = h.bake(&stack, 1);
        let slot = baked.draws[0].slot;
        assert_ne!(slot, 0, "the effect draw is not the layer's own");

        for (x, y) in [(20, 32), (14, 26), (30, 12), (8, 8), (24, 20)] {
            let a = slice_alpha(&h, slot, x, y);
            for (mx, my) in [
                (DOC - 1 - x, y),
                (x, DOC - 1 - y),
                (DOC - 1 - x, DOC - 1 - y),
            ] {
                let b = slice_alpha(&h, slot, mx, my);
                assert!(
                    a.abs_diff(b) <= 1,
                    "softness {softness}: ({x},{y})={a} against ({mx},{my})={b}: \
                     the blur is lopsided"
                );
            }
        }
        // And it actually softened something, or the symmetry above is the
        // symmetry of an empty texture.
        let ramp: Vec<u8> = (8..24).map(|x| slice_alpha(&h, slot, x, 32)).collect();
        assert!(
            ramp.windows(2).all(|w| w[0] <= w[1]) && ramp[0] < ramp[15],
            "softness {softness}: no falloff in the shadow: {ramp:?}"
        );
    }
}

/// A shadow of a layer that runs to the edge of the canvas fades **at** that
/// edge.
///
/// The rule the selection's feather already keeps in as many words — "outside the
/// canvas counts as unselected, so a selection against the document edge fades at
/// it, as Photoshop's and GIMP's do" — and the one a `textureLoad` most naturally
/// gets wrong, because clamp-to-edge makes a box pass sum the border row over and
/// over. Matching the feather's kernel and then not matching its boundary is the
/// worse half of both.
///
/// The shape is a band flush against the **left** edge and spanning the whole
/// height, which is what makes the reading discriminating. Being uniform in `y`,
/// its shadow is the same all the way down *if* the vertical box pass replicates
/// the border row, and weaker at the top and bottom rows if it reads zero — so one
/// comparison between the middle of an edge column and the middle of the canvas
/// answers the question, with no value worked out by hand.
///
/// The first draft of this test put the band along the top and asserted that the
/// knockout reached row zero. It passed under both rules: the knockout is
/// `1 - coverage` and the band covers that row either way, so the assertion was
/// about something else entirely.
#[test]
fn a_shadow_of_a_layer_against_the_canvas_edge_fades_at_it() {
    let mut h = harness_or_skip!();
    h.write_block(
        0,
        PixelRect {
            x: 0,
            y: 0,
            width: 20,
            height: DOC,
        },
        [255, 255, 255, 255],
    );
    let draw = layer(0, 1.0, BlendMode::Normal);
    let soft = [Effect {
        spread: 3.0,
        softness: 9.0,
        distance: 0.0,
        ..Effect::drop_shadow()
    }];
    let baked = h.bake(&[effected(draw, &soft)], 1);
    let slot = baked.draws[0].slot;

    // Right of the band, where the shadow reaches into open canvas: a real
    // falloff, which is what says the shadow exists at all.
    let across: Vec<u8> = (20..40).map(|x| slice_alpha(&h, slot, x, 32)).collect();
    assert!(
        across[0] > across[19] && across[0] > 0,
        "no shadow beside the band: {across:?}"
    );

    // The same column at the top and bottom rows. Half the tent's support is off
    // the canvas there, so the shadow has to be plainly weaker than in the middle.
    // `u32`, because three quarters of a `u8` over 85 is not a `u8`.
    let middle = u32::from(slice_alpha(&h, slot, 24, DOC / 2));
    for y in [0, DOC - 1] {
        let edge = u32::from(slice_alpha(&h, slot, 24, y));
        assert!(
            edge < middle * 3 / 4,
            "row {y} reads {edge} against {middle} in the middle: the kernel is \
             replicating the border instead of fading at it"
        );
    }
}

/// The distance field is a **disc**, not a square.
///
/// §3.1's whole argument: a separated `max` — a horizontal dilate then a
/// vertical one — grows to a square, and on a diagonal the corner is out by
/// `r(sqrt 2 - 1)`, which is 41% of the radius. The corner of the shape is the
/// place the two methods disagree most, so that is where this reads.
///
/// The second assertion is the discriminating one. A square dilate of radius 12
/// from the corner texel at (24, 24) reaches (12, 12) on both axes at once, so it
/// would put full coverage at (14, 14) — which is 14.1 texels from the shape and
/// must be outside a disc of 12.
#[test]
fn the_distance_field_is_a_disc_and_not_a_square() {
    let mut h = harness_or_skip!();
    h.write_block(0, SHAPE, [255, 255, 255, 255]);

    let draw = layer(0, 1.0, BlendMode::Normal);
    let ring = [outline(Color::WHITE, 12.0, OutlinePosition::Outside)];
    let stack = [effected(draw, &ring)];
    let baked = h.bake(&stack, 1);
    let slot = baked.draws[0].slot;

    // Straight out from an edge, well inside the width: solid.
    assert_eq!(
        slice_alpha(&h, slot, 16, 32),
        255,
        "the outline is missing where it is only 8 texels out"
    );
    // Diagonally out from the corner at 8.5 texels: inside a disc of 12.
    assert!(
        slice_alpha(&h, slot, 18, 18) > 250,
        "the disc does not reach its own corner: {}",
        slice_alpha(&h, slot, 18, 18)
    );
    // Diagonally out at 14.1 texels: outside a disc of 12, inside a square.
    assert_eq!(
        slice_alpha(&h, slot, 14, 14),
        0,
        "the field is a square: it painted 14.1 texels out from a corner at a \
         width of 12"
    );
    // And plainly outside on the axis too, so the reading above is about the
    // shape of the field rather than about a width that came out short.
    assert_eq!(slice_alpha(&h, slot, 10, 32), 0);
}

/// An inner effect is confined to the layer's alpha, and it is
/// `LayerDraw::clipped` that does it.
///
/// **No new mechanism at all** — §3.3's other half. `clipped` already means
/// "bounded by the alpha of the nearest unclipped layer below", and an inner
/// effect drawn immediately above its own layer reads exactly that. The
/// asymmetry with the outer effect's baked knockout is the thing that gets
/// forgotten and reintroduced as a uniform, so both directions are asserted:
/// the band is inside the shape, and nothing of it is outside.
#[test]
fn an_inner_outline_is_confined_to_the_layers_own_alpha() {
    let mut h = harness_or_skip!();
    h.write_block(1, WHOLE, [0, 0, 0, 255]);
    h.write_block(0, SHAPE, [255, 255, 255, 255]);

    let floor = layer(1, 1.0, BlendMode::Normal);
    let draw = layer(0, 1.0, BlendMode::Normal);
    let inside = [outline(
        Color::new(1.0, 0.0, 0.0, 1.0),
        4.0,
        OutlinePosition::Inside,
    )];
    let stack = [effected(floor, &[]), effected(draw, &inside)];
    let baked = h.bake(&stack, 2);

    // The layer, then its inner effect over it: an inside outline is the one
    // effect that composites *above* the layer it came from.
    assert_eq!(baked.draws.len(), 3);
    assert_eq!(baked.draws[1].slot, 0, "{:?}", baked.draws);
    assert!(
        baked.draws[2].clipped,
        "an inner effect must carry the clip"
    );

    // Two texels in from the edge: the band.
    assert_near(
        h.composite_pixel(&baked.draws, 26, 32),
        [255, 0, 0],
        2,
        "the inside outline is missing at the edge it traces",
    );
    // Eight texels in: past the band, so the layer's own white.
    assert_near(
        h.composite_pixel(&baked.draws, 32, 32),
        [255, 255, 255],
        2,
        "the inside outline filled the whole shape",
    );
    // Outside the shape: the black floor and nothing else. This is the half the
    // clip flag is doing, and it is the half that fails if the flag is dropped —
    // an unclipped band would paint the whole canvas red.
    assert_near(
        h.composite_pixel(&baked.draws, 12, 32),
        [0, 0, 0],
        2,
        "the inside outline escaped its own layer",
    );
}

/// **Each outline position draws the width it claims, on the side it claims.**
///
/// The three are one measurement taken three ways, off the effect slice's own
/// alpha, along a row through the middle of a square whose left edge is at x = 24.
/// A band's extent is what the position *means*, so this is the test that would
/// have caught a Centre that was quietly an Outside of half the width — which is
/// what a blanket knockout forced, and is why `docs/layer-effects.md` §3.3 was
/// reversed: that control is Photoshop's "Layer knocks out **drop shadow**" and it
/// is named for the drop shadow because it is the drop shadow's.
///
/// Width 8 throughout, so Centre reaches 4 each side and the three are told apart
/// by where the band sits rather than by how wide it is.
///
/// **On a shape 48 texels across rather than [`SHAPE`]'s 16, and that is the whole
/// reason this test caught anything.** A band of 8 inwards *fills* a 16-wide
/// square — every interior texel is within 8 of some edge — so an Inside
/// assertion against that shape cannot tell a band from a fill, and the middle
/// assertion below would be vacuous. It is also why each span is checked to be a
/// band and not merely to start in the right place.
#[test]
fn each_outline_position_draws_the_band_it_claims() {
    let mut h = harness_or_skip!();
    let wide = PixelRect {
        x: 8,
        y: 8,
        width: 48,
        height: 48,
    };
    h.write_block(0, wide, [255, 255, 255, 255]);
    let draw = layer(0, 1.0, BlendMode::Normal);

    // The run of texels along y = 32, up to the middle of the canvas, where the
    // effect has alpha. The **left half only**: the shape has a right edge with a
    // band of its own, and reading the whole row would report the two bands and
    // the gap between them as one span.
    let span = |h: &Harness, slot: u32| -> Option<(u32, u32)> {
        let lit: Vec<u32> = (0..32)
            .filter(|x| slice_alpha(h, slot, *x, 32) > 128)
            .collect();
        Some((*lit.first()?, *lit.last()?))
    };

    // Outside: entirely beyond the edge, so it ends where the shape begins.
    let ring = [outline(Color::WHITE, 8.0, OutlinePosition::Outside)];
    let baked = h.bake(&[effected(draw, &ring)], 1);
    let outside = span(&h, baked.draws[0].slot).expect("an outside band");
    assert_eq!(
        outside,
        (0, 7),
        "an outside band of 8 should run 0..7, up to but not into the shape"
    );

    // Centre: half in and half out, so it straddles x = 8. Four each side.
    let ring = [outline(Color::WHITE, 8.0, OutlinePosition::Centre)];
    let baked = h.bake(&[effected(draw, &ring)], 1);
    let centre = span(&h, baked.draws[0].slot).expect("a centred band");
    assert_eq!(
        centre,
        (4, 11),
        "a centred band of 8 should straddle the edge at 8, four each side — a \
         band of (0, 7) is an Outside wearing Centre's name, which is what a \
         blanket knockout forced"
    );

    // Inside: entirely within the shape — and **the last draw, not the first.**
    //
    // An inside outline is the one position that composites *above* its layer, so
    // `draws` is `[the layer, the effect]` where the other two are
    // `[the effect, the layer]`. Reading `draws[0]` here reads the layer's own
    // slice, whose first lit texel is the shape's own edge — which is the number
    // this assertion wanted. **It passed by coincidence**, and marking
    // `fs_grow`'s `SHAPE_INNER` arm with a constant is what proved it: the output
    // did not move at all. A test that reads the wrong texture and agrees anyway
    // is worth more scars than one that fails.
    let ring = [outline(Color::WHITE, 8.0, OutlinePosition::Inside)];
    let baked = h.bake(&[effected(draw, &ring)], 1);
    assert_eq!(baked.draws[0].slot, 0, "the layer composites first");
    let slot = baked.draws[1].slot;
    assert_ne!(slot, 0, "the inner effect has a slice of its own");
    let inside: Vec<u32> = (8..32)
        .filter(|x| slice_alpha(&h, slot, *x, 32) > 128)
        .collect();
    assert_eq!(
        (inside[0], *inside.last().expect("an inside band")),
        (8, 15),
        "an inside band of 8 should run 8..15, from the edge inwards"
    );
    // **A band and not a fill**, which is the assertion a 16-wide shape could not
    // make: 32 is 24 texels from either edge, so an inward band of 8 must not
    // reach it.
    assert_eq!(
        slice_alpha(&h, slot, 32, 32),
        0,
        "the inward band filled the shape instead of tracing its edge"
    );

    // **And the slice is deliberately lit *outside* the shape**, which is not a
    // defect and is why the scan above starts at the edge. `SHAPE_INNER` returns
    // the inward band whole: the flood is seeded on the *complement*, so an
    // uncovered texel seeds itself, reads a distance of zero and comes out at full
    // coverage. `LayerDraw::clipped` is what bounds it — the asymmetry §3.3's
    // reversal keeps — and multiplying by the coverage here as well would apply
    // the layer's alpha twice. `an_inner_outline_is_confined_to_the_layers_own_
    // alpha` is the composite half of this and asserts the band does not escape.
    assert_eq!(
        slice_alpha(&h, slot, 2, 32),
        255,
        "the inward band is unconfined in the slice; the clip flag is what \
         bounds it, and a slice that were already bounded would be applying the \
         layer's alpha twice"
    );
    assert!(
        baked.draws[1].clipped,
        "and nothing bounds it unless the draw carries the clip"
    );
}

/// **Only a drop shadow knocks its own layer out.**
///
/// The reversal of §3.3 stated as the thing it makes possible: a centred outline
/// survives under an opaque layer's own shape, where a knockout applied to
/// everything compositing below the layer would have removed exactly the half
/// that makes it centred. Read off the effect slice, because the composite would
/// hide the inner half under the layer — which is the *point*: it is hidden by
/// ordinary compositing rather than deleted at bake time, so it comes back
/// through a translucent layer.
///
/// The shadow's half is asserted beside it, so the test cannot pass by the
/// knockout having been removed altogether.
#[test]
fn the_knockout_belongs_to_the_drop_shadow_alone() {
    let mut h = harness_or_skip!();
    h.write_block(0, SHAPE, [255, 255, 255, 255]);
    let draw = layer(0, 1.0, BlendMode::Normal);

    // Well inside the shape, where a knockout leaves nothing.
    let (x, y) = (32, 32);

    // A shadow grown past the shape: knocked out inside it.
    let shadow = [Effect {
        spread: 8.0,
        softness: 0.0,
        distance: 0.0,
        ..Effect::drop_shadow()
    }];
    let baked = h.bake(&[effected(draw, &shadow)], 1);
    assert_eq!(
        slice_alpha(&h, baked.draws[0].slot, x, y),
        0,
        "a drop shadow must be knocked out under its own shape"
    );

    // A centred outline wide enough to reach the same texel: **not** knocked out.
    // Sixteen wide, so its inner half reaches eight texels in from x = 24.
    let ring = [outline(Color::WHITE, 16.0, OutlinePosition::Centre)];
    let baked = h.bake(&[effected(draw, &ring)], 1);
    assert!(
        slice_alpha(&h, baked.draws[0].slot, x, y) > 128,
        "a centred outline's inner half was knocked out: it is hidden by the \
         layer at composite time, not removed at bake time"
    );
}

/// The cache rebakes when the pixels or the parameters move, and not otherwise.
///
/// The whole of §5's contract, and it is invisible any other way: a stale bake
/// and a fresh one of the same parameters produce the same pixels, so only a
/// count can say which happened. The last two steps are the ones worth having —
/// the opacity and the blend mode are the *draw's*, applied by the composite, so
/// dragging either must cost nothing.
#[test]
fn an_effect_is_rebaked_when_its_pixels_move_and_not_otherwise() {
    let mut h = harness_or_skip!();
    h.write_block(0, SHAPE, [255, 255, 255, 255]);

    let draw = layer(0, 1.0, BlendMode::Normal);
    let mut effects = vec![outline(Color::WHITE, 4.0, OutlinePosition::Outside)];

    let baked = h.bake(&[effected(draw, &effects)], 1);
    let slot = baked.draws[0].slot;
    assert_eq!(h.canvas.effect_bakes(), 1);

    h.bake(&[effected(draw, &effects)], 1);
    assert_eq!(
        h.canvas.effect_bakes(),
        1,
        "an unchanged effect was rebaked"
    );

    // The layer's own pixels: `slot_revision` is bumped inside every method that
    // writes a slice, which is what makes this exhaustive by construction.
    h.write_block(
        0,
        PixelRect {
            x: 8,
            y: 8,
            width: 4,
            height: 4,
        },
        [255, 255, 255, 255],
    );
    h.bake(&[effected(draw, &effects)], 1);
    assert_eq!(h.canvas.effect_bakes(), 2, "a layer edit did not rebake");

    effects[0].spread = 6.0;
    h.bake(&[effected(draw, &effects)], 1);
    assert_eq!(
        h.canvas.effect_bakes(),
        3,
        "a parameter change did not rebake"
    );
    assert_eq!(
        h.bake(&[effected(draw, &effects)], 1).draws[0].slot,
        slot,
        "the effect moved slice for nothing"
    );

    effects[0].opacity = 0.25;
    effects[0].blend = BlendMode::Screen;
    let baked = h.bake(&[effected(draw, &effects)], 1);
    assert_eq!(
        h.canvas.effect_bakes(),
        3,
        "opacity and blend are the draw's, not the bake's"
    );
    assert_eq!(baked.draws[0].blend, BlendMode::Screen.index());
    assert!((baked.draws[0].opacity - 0.25).abs() < 1e-6);
}

/// Over budget the draw path drops effects in a stated order and says how many.
///
/// §6.1a: adding an effect is gated by the model, but an undo, an import or a
/// document opened from a file can all arrive over budget, and there is no answer
/// to "your undo does not fit" better than doing it. So the draw path degrades
/// **visibly** — the ones furthest down the stack go first, so the layer somebody
/// is working on keeps its own, and `effects_dropped` is what the panel reports.
/// Truncating the list at the cap instead would be the silent version.
///
/// Reached through `base` rather than through 128 effects on 64 layers: the
/// budget is `MAX_EFFECT_SLICES` against what is left under the device's
/// 256-slice ceiling, so a base near the top exercises the same arithmetic on a
/// stack a test can build.
#[test]
fn an_effect_over_budget_is_dropped_from_the_bottom_and_counted() {
    let mut h = harness_or_skip!();
    h.write_block(0, SHAPE, [255, 255, 255, 255]);
    h.write_block(
        1,
        PixelRect {
            x: 8,
            y: 8,
            width: 8,
            height: 8,
        },
        [255, 255, 255, 255],
    );

    let bottom = layer(0, 1.0, BlendMode::Normal);
    let top = layer(1, 1.0, BlendMode::Normal);
    let ring = [outline(Color::WHITE, 3.0, OutlinePosition::Outside)];
    let stack = [effected(bottom, &ring), effected(top, &ring)];

    // One slice left under the ceiling.
    let baked = h.bake(&stack, 255);
    assert_eq!(baked.dropped, 1, "{:?}", baked.draws);
    assert_eq!(h.canvas.effects_dropped(), 1);
    // Three draws, not four: the two layers and the surviving effect, which is
    // the *upper* layer's.
    assert_eq!(baked.draws.len(), 3, "{:?}", baked.draws);
    assert_eq!(baked.draws[0].slot, 0, "the bottom layer lost its effect");
    assert_eq!(baked.draws[1].slot, 255, "the top layer kept its own");
    assert_eq!(baked.draws[2].slot, 1);

    // **No slices left at all**, which is reachable: `SlotPool` hands out up to
    // slot 255, so `slot_capacity_needed` can reach 256 and the base 257 — enough
    // delete-then-add cycles get there through parked slices. Both figures are
    // over the array's own ceiling, and falling through would ask `ensure_slots`
    // for a 257th slice: a `debug_assert` on the drawing path, or in a release
    // build a fresh 256-slice array allocated and copied every frame.
    for base in [256, 257] {
        let baked = h.bake(&stack, base);
        assert_eq!(baked.draws.len(), 2, "base {base}: {:?}", baked.draws);
        assert_eq!(baked.dropped, 2, "base {base} did not say what it dropped");
        assert_eq!(h.canvas.effects_dropped(), 2);
    }
}

/// A shadow baked mid-stroke is the shadow the commit produces.
///
/// The bake extracts the layer's coverage after its mask **and after the wet
/// stroke**, which is the one place in it that has to agree with
/// `composite.wgsl` — and it agrees about alpha only, because an effect is a
/// shape wearing a colour of its own. Nothing but a test can hold those two
/// together: the pixels are identical when they agree and there is no shared
/// function to point at.
///
/// One level of slack, because the committed layer has been through an 8-bit
/// store where the scratch had not.
#[test]
fn a_live_stroke_bakes_the_shadow_the_commit_would() {
    let mut h = harness_or_skip!();
    let draw = layer(0, 1.0, BlendMode::Normal);
    let ring = [outline(Color::WHITE, 5.0, OutlinePosition::Outside)];
    let style = StrokeStyle {
        color: Color::WHITE,
        opacity: 1.0,
        ..Default::default()
    };

    h.stamp(&[dab(32.0, 32.0, 10.0, 1.0)]);
    let live = h.bake_frame(
        &[effected(draw, &ring)],
        1,
        EffectFrame {
            active_index: 0,
            stroke: style,
            stroke_live: true,
        },
    );
    let slot = live.draws[0].slot;
    let wet: Vec<u8> = (10..54).map(|x| slice_alpha(&h, slot, x, 32)).collect();
    assert!(
        wet.iter().any(|a| *a > 200),
        "the wet stroke did not reach the bake at all: {wet:?}"
    );

    h.commit(Color::WHITE, 1.0, BrushMode::Paint);
    let dry = h.bake_frame(
        &[effected(draw, &ring)],
        1,
        EffectFrame {
            active_index: 0,
            stroke: StrokeStyle {
                opacity: 0.0,
                ..style
            },
            stroke_live: false,
        },
    );
    assert_eq!(dry.draws[0].slot, slot);
    let baked: Vec<u8> = (10..54).map(|x| slice_alpha(&h, slot, x, 32)).collect();
    for (i, (a, b)) in wet.iter().zip(&baked).enumerate() {
        assert!(
            a.abs_diff(*b) <= 1,
            "x={}: mid-stroke {a} against committed {b}",
            10 + i
        );
    }
}

/// A dragged float carries its own effects, and puts them down where the pixels
/// landed.
///
/// §5.2's whole case, and the reason the cache is keyed on the slot the *draw*
/// carries rather than on the layer: during a drag the draw carries the preview
/// slice, so the effect baked from it is a different entry from the one baked
/// from the layer's own — and the commit swaps back to an entry that is stale for
/// the ordinary reason. No rule about floats anywhere in the cache.
///
/// It caught a real defect. `render_float` writes the preview slice and did not
/// move its revision, so the outline baked at the moment the pixels were picked
/// up stayed there for the whole drag — a shadow left behind by a dragged object,
/// which is the exact failure §5.2 names.
#[test]
fn a_dragged_float_carries_the_effect_derived_from_it() {
    let mut h = harness_or_skip!();
    let block = PixelRect {
        x: 8,
        y: 8,
        width: 10,
        height: 10,
    };
    h.write_block(0, block, [255, 255, 255, 255]);

    let mut xf = Transform::identity(block);
    let preview = h
        .canvas
        .begin_float(
            &h.gpu.device,
            &h.gpu.queue,
            1,
            &FloatSource {
                slot: 0,
                rect: block,
                pixels: None,
                mask: None,
            },
        )
        .expect("no room for a preview");
    // `Editor::effected_draws` substitutes the preview slice for the layer's, so
    // the bake is handed exactly what the composite is handed.
    let dragged = LayerDraw {
        slot: preview,
        ..layer(0, 1.0, BlendMode::Normal)
    };
    let ring = [outline(Color::WHITE, 4.0, OutlinePosition::Outside)];

    // A frame of the drag at identity first. A *lift* takes the pixels out of the
    // preview, so until something has been drawn into it the preview holds only
    // the hole — and an outline of nothing is nothing, which would make the
    // assertions below pass for the wrong reason.
    let at_rest = FloatParams {
        inverse: xf.inverse(),
        dest: xf.dest_rect(UVec2::splat(DOC)),
    };
    let mut enc = h.encoder();
    h.canvas.draw_float(&h.gpu.queue, &mut enc, &at_rest);
    h.gpu.queue.submit(Some(enc.finish()));

    let baked = h.bake(&[effected(dragged, &ring)], preview + 1);
    let slot = baked.draws[0].slot;
    assert_ne!(slot, preview, "the effect took the float's own slice");
    assert!(
        slice_alpha(&h, slot, 8, 5) > 0,
        "the outline is missing where the picture is"
    );

    // Drag it clean across the canvas.
    xf.offset = Vec2::splat(28.0);
    let params = FloatParams {
        inverse: xf.inverse(),
        dest: xf.dest_rect(UVec2::splat(DOC)),
    };
    let mut enc = h.encoder();
    h.canvas.draw_float(&h.gpu.queue, &mut enc, &params);
    h.gpu.queue.submit(Some(enc.finish()));

    h.bake(&[effected(dragged, &ring)], preview + 1);
    assert!(
        slice_alpha(&h, slot, 36, 33) > 0,
        "the effect did not follow the float"
    );
    assert_eq!(
        slice_alpha(&h, slot, 8, 5),
        0,
        "the effect was left behind where the picture used to be"
    );

    // The commit puts the pixels in the layer and the draw goes back to the
    // layer's own slice, which is a different cache key and stale for the
    // ordinary reason — the commit moved that slice's revision.
    let damage = xf.damage(UVec2::splat(DOC), true).expect("something to do");
    let mut enc = h.encoder();
    h.canvas
        .commit_float(&h.gpu.queue, &mut enc, damage, &params);
    h.gpu.queue.submit(Some(enc.finish()));
    h.canvas.end_float(&h.gpu.queue);

    let settled = layer(0, 1.0, BlendMode::Normal);
    let after = h.bake(&[effected(settled, &ring)], preview + 1);
    let slot = after.draws[0].slot;
    assert!(
        slice_alpha(&h, slot, 36, 33) > 0,
        "the committed picture has no outline"
    );
    assert_eq!(
        slice_alpha(&h, slot, 8, 5),
        0,
        "the outline of where the picture came from survived the commit"
    );
}

/// A spread wider than the canvas, and one that is not a number at all, are
/// baked rather than crashed on.
///
/// `spread` is an `f32` a document carries, so "nobody would type that" is not a
/// bound. Unbounded, the flood's step count is a shift by `32 - leading_zeros` of
/// a saturating cast, which at a spread near four billion is a shift of 32 — a
/// panic, on the drawing path, out of a file. The step count is bounded by the
/// longest side of the canvas instead, which reaches every texel anyway.
///
/// The assertion is only that it produced a picture and did not die; what the
/// picture *is* for a NaN spread is not a thing worth pinning.
#[test]
fn an_absurd_spread_is_baked_rather_than_crashed_on() {
    let mut h = harness_or_skip!();
    h.write_block(0, SHAPE, [255, 255, 255, 255]);
    let draw = layer(0, 1.0, BlendMode::Normal);

    for spread in [1.0e9, f32::MAX, f32::INFINITY, f32::NAN] {
        let ring = [outline(Color::WHITE, spread, OutlinePosition::Outside)];
        let baked = h.bake(&[effected(draw, &ring)], 1);
        assert_eq!(baked.draws.len(), 2, "spread {spread} produced no draw");
        // A spread that covers the canvas leaves nothing for the outline to
        // occupy once the knockout has taken the shape out of it, so this reads
        // that the pass ran rather than what it wrote.
        let _ = slice_alpha(&h, baked.draws[0].slot, 4, 4);
    }
}

/// The shadow follows the brush: a live stroke rebakes on **every** frame.
///
/// §5.1's whole argument, and the thing the design says makes the difference
/// between a feature and a limitation — "drawing an outlined shape and not
/// seeing the outline until you lift is the kind of thing that makes a feature
/// unusable rather than merely limited". Nothing else here catches it: the
/// stamp's slice revisions do not move until the commit and its parameters never
/// move at all, so an ordinary freshness check would find the entry fresh from
/// the second frame on and freeze the shadow where the pen went down.
#[test]
fn a_live_stroke_rebakes_the_effect_on_every_frame() {
    let mut h = harness_or_skip!();
    let draw = layer(0, 1.0, BlendMode::Normal);
    let ring = [outline(Color::WHITE, 4.0, OutlinePosition::Outside)];
    let style = StrokeStyle {
        color: Color::WHITE,
        opacity: 1.0,
        ..Default::default()
    };
    let live = EffectFrame {
        active_index: 0,
        stroke: style,
        stroke_live: true,
    };

    h.stamp(&[dab(16.0, 32.0, 8.0, 1.0)]);
    let first = h.bake_frame(&[effected(draw, &ring)], 1, live);
    let slot = first.draws[0].slot;
    let bakes = h.canvas.effect_bakes();
    assert!(
        slice_alpha(&h, slot, 16, 21) > 0,
        "the first frame of the stroke did not reach the bake"
    );
    assert_eq!(
        slice_alpha(&h, slot, 48, 32),
        0,
        "the far end of the stroke has not been painted yet"
    );

    // The next frame of the same stroke: more dabs in the scratch, nothing else
    // changed anywhere.
    h.stamp(&[dab(48.0, 32.0, 8.0, 1.0)]);
    h.bake_frame(&[effected(draw, &ring)], 1, live);
    assert!(
        h.canvas.effect_bakes() > bakes,
        "the second frame of a stroke did not rebake"
    );
    assert!(
        slice_alpha(&h, slot, 48, 21) > 0,
        "the shadow did not follow the brush: the outline is missing where the \
         stroke has just been"
    );
}

/// Ending a stroke without committing it still rebakes.
///
/// A cancel writes no pixels, so no slice revision moves and every other part of
/// the stamp is unchanged — the bake would keep showing a stroke the artist threw
/// away. `CachedEffect::live` is what catches it, and this is the only thing that
/// says so.
#[test]
fn a_cancelled_stroke_rebakes_the_effect_it_was_showing() {
    let mut h = harness_or_skip!();
    h.write_block(0, SHAPE, [255, 255, 255, 255]);
    let draw = layer(0, 1.0, BlendMode::Normal);
    let ring = [outline(Color::WHITE, 4.0, OutlinePosition::Outside)];
    let style = StrokeStyle {
        color: Color::WHITE,
        opacity: 1.0,
        ..Default::default()
    };

    h.stamp(&[dab(8.0, 8.0, 6.0, 1.0)]);
    let live = h.bake_frame(
        &[effected(draw, &ring)],
        1,
        EffectFrame {
            active_index: 0,
            stroke: style,
            stroke_live: true,
        },
    );
    let slot = live.draws[0].slot;
    assert!(
        slice_alpha(&h, slot, 8, 16) > 0,
        "the stroke's own outline never appeared"
    );

    // Cancelled, not committed: the scratch is thrown away and the layer is
    // untouched.
    let mut enc = h.encoder();
    h.canvas.clear_stroke(&h.gpu.device, &mut enc);
    h.gpu.queue.submit(Some(enc.finish()));
    let before = h.canvas.effect_bakes();
    h.bake(&[effected(draw, &ring)], 1);
    assert!(
        h.canvas.effect_bakes() > before,
        "a cancelled stroke left the effect showing it"
    );
    assert_eq!(
        slice_alpha(&h, slot, 8, 16),
        0,
        "the cancelled stroke is still in the effect slice"
    );
}

/// **Every mode the interface offers actually reaches the shader.**
///
/// `blend_rgb`'s `switch` falls through to `default`, which is Normal — so a
/// variant added to `BlendMode` with no `case` for it composites as Normal,
/// silently, and the only symptom is a dropdown entry that does nothing. The
/// Rust half of that pair is `all_lists_every_blend_mode`, which cannot see the
/// shader at all.
///
/// Driven off `BlendMode::ALL`, so a mode added to the enum is one this starts
/// checking without anybody remembering to.
///
/// The backdrop and the source are chosen so that **no mode is the identity on
/// them and no two need to be told apart**: what is asserted is only that each
/// non-Normal mode moves the picture somewhere Normal does not. Asserting each
/// mode's own arithmetic here would be restating `blend.wgsl` in Rust, which is
/// the second implementation this file exists to avoid — the formulas are the
/// W3C ones and their *identities* are what `a_blended_brush_keeps_the_
/// identities_its_mode_promises` pins.
///
/// Two are expected to agree with Normal and are named rather than skipped
/// quietly: nothing is a coincidence here.
#[test]
fn every_blend_mode_moves_the_picture() {
    let mut h = harness_or_skip!();

    // Mid greys with all three channels different, so a mode that only touches
    // luminosity and one that only touches hue both show up.
    let under = [70u8, 160, 110, 255];
    let over = [180u8, 90, 200, 255];

    reset(&mut h);
    let rect = whole(&h);
    h.write_block(0, rect, under);
    h.write_block(1, rect, over);
    let normal = h.composite_pixel(
        &[
            layer(0, 1.0, BlendMode::Normal),
            layer(1, 1.0, BlendMode::Normal),
        ],
        32,
        32,
    );

    let mut unmoved = Vec::new();
    for mode in every_blend_mode() {
        if mode == BlendMode::Normal {
            continue;
        }
        let got = h.composite_pixel(
            &[layer(0, 1.0, BlendMode::Normal), layer(1, 1.0, mode)],
            32,
            32,
        );
        if (0..3).all(|i| got[i].abs_diff(normal[i]) <= 1) {
            unmoved.push(mode);
        }
    }

    assert!(
        unmoved.is_empty(),
        "these modes composited as Normal, so they have no arm in blend.wgsl: {unmoved:?}"
    );
}

/// The four non-separable modes are the ones with helpers behind them, and a
/// helper that is wrong is easiest to see in the identities the modes promise.
///
/// Luminosity of a colour onto itself is that colour; Colour of a colour onto
/// itself likewise. Both go through `set_lum`, `clip_color` and — for Hue and
/// Saturation — `set_sat`, so a broken helper cannot pass these.
#[test]
fn the_colour_modes_keep_the_identities_they_promise() {
    let mut h = harness_or_skip!();
    let same = [120u8, 90, 200, 255];

    reset(&mut h);
    let rect = whole(&h);
    h.write_block(0, rect, same);
    h.write_block(1, rect, same);

    for mode in [
        BlendMode::Hue,
        BlendMode::Saturation,
        BlendMode::Color,
        BlendMode::Luminosity,
    ] {
        let got = h.composite_pixel(
            &[layer(0, 1.0, BlendMode::Normal), layer(1, 1.0, mode)],
            32,
            32,
        );
        assert_near(
            got,
            [same[0], same[1], same[2]],
            2,
            &format!("{mode:?} of a colour onto itself"),
        );
    }
}

/// **Add (Glow) is Porter-Duff `plus`, and it agrees with Add over an opaque
/// backdrop.**
///
/// That agreement is derived rather than hoped for — `composite_over`'s note
/// works it out — and it is the property worth pinning, because it is what
/// says the new mode did not change any of the twenty-five real Clip Studio
/// layers that used to arrive as Add. Where the two *can* differ is a partly
/// transparent backdrop, which is the second half.
///
/// This is also the one mode with no `blend_rgb` arm, so a refactor that moved
/// it there and dropped the compositing branch would fail here rather than in
/// somebody's document.
#[test]
fn add_glow_matches_add_over_an_opaque_backdrop_and_differs_over_a_soft_one() {
    let mut h = harness_or_skip!();
    let rect = whole(&h);

    // Opaque backdrop: the two must agree.
    reset(&mut h);
    h.write_block(0, rect, [70, 160, 110, 255]);
    h.write_block(1, rect, [90, 40, 120, 255]);
    let as_add = h.composite_pixel(
        &[
            layer(0, 1.0, BlendMode::Normal),
            layer(1, 1.0, BlendMode::Add),
        ],
        32,
        32,
    );
    let as_glow = h.composite_pixel(
        &[
            layer(0, 1.0, BlendMode::Normal),
            layer(1, 1.0, BlendMode::AddGlow),
        ],
        32,
        32,
    );
    assert_near(
        as_glow,
        [as_add[0], as_add[1], as_add[2]],
        1,
        "Add (Glow) over an opaque backdrop is Add",
    );

    // Partly transparent backdrop: this is where the operator shows itself.
    // `plus` adds the premultiplied backdrop straight on, so the result is
    // lighter than the general form's, which scales the blend by the backdrop
    // alpha and then lays the source over what is left.
    reset(&mut h);
    h.write_block(0, rect, [70, 160, 110, 128]);
    h.write_block(1, rect, [90, 40, 120, 128]);
    let soft_add = h.composite_pixel(
        &[
            layer(0, 1.0, BlendMode::Normal),
            layer(1, 1.0, BlendMode::Add),
        ],
        32,
        32,
    );
    let soft_glow = h.composite_pixel(
        &[
            layer(0, 1.0, BlendMode::Normal),
            layer(1, 1.0, BlendMode::AddGlow),
        ],
        32,
        32,
    );
    assert_ne!(
        soft_glow, soft_add,
        "over a soft edge the two operators have to part company"
    );
    assert!(
        soft_glow[3] >= soft_add[3],
        "`plus` adds alpha as well as colour: {soft_glow:?} against {soft_add:?}"
    );
}

// ---------------------------------------------------------------------------
// The tile atlas
//
// A layer's texels live in 256-square tiles of a page atlas, and a page table
// says where each one is. Residency is the identity today — page `n` holds slot
// `n`'s tiles at their own coordinates — so nothing below could be told from the
// dense array it replaced *unless* the table is deliberately rearranged. That is
// what `unback_tile_for_test` and `borrow_tile_for_test` are for, and they are
// the only thing that runs the substitution or the cross-tile tap at all.
// ---------------------------------------------------------------------------

/// A canvas three tiles wide and two tall, and **a whole number of neither**.
///
/// The harness's 64 is one tile, so it has no boundary to straddle and no second
/// tile to be pointed at. Two further properties are load-bearing and both were
/// arrived at by mutation rather than by design:
///
/// * **Non-square.** With a square tile grid an x/y transposition anywhere in
///   the packing — `Entry::cell`, the WGSL's `tile_atlas_texel` — resolves to a
///   tile that exists and, on a slot filled flat, holds the same thing, so no
///   assertion can see it. Three by two sends tile (2, 0) to (0, 2), which is
///   off the bottom of the page and reads as zero.
/// * **Not a multiple of the tile.** The page is the canvas rounded up, so 700 ×
///   500 leaves a margin out to 768 × 512 that is cleared and never written. A
///   tap that fails to clamp at the canvas edge reads *that*, and comes back
///   short. On a canvas that filled its page exactly, the unclamped tap runs off
///   the tile grid instead, the page table's out-of-bounds `textureLoad` answers
///   **zero — a legitimate entry, page 0 tile (0, 0)** — and the answer is
///   plausible content rather than nothing. Dropping the clamp passed against a
///   768 × 512 fixture.
const TILED_W: u32 = 700;
const TILED_H: u32 = 500;

/// A renderer over the `TILED_W` × `TILED_H` canvas with `slots` slices, cleared.
fn tiled_canvas(gpu: &Gpu, slots: u32) -> CanvasRenderer {
    let mut canvas = CanvasRenderer::new(
        &gpu.device,
        &gpu.queue,
        UVec2::new(TILED_W, TILED_H),
        TARGET_FORMAT,
        slots,
    );
    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    canvas.clear_all_layers(&gpu.queue);
    canvas.clear_stroke(&gpu.device, &mut enc);
    gpu.queue.submit(Some(enc.finish()));
    canvas
}

fn fill_tiled_slot(gpu: &Gpu, canvas: &mut CanvasRenderer, slot: u32, rgba: [u8; 4]) {
    let bytes: Vec<u8> = rgba
        .iter()
        .copied()
        .cycle()
        .take((TILED_W as usize) * (TILED_H as usize) * 4)
        .collect();
    canvas.write_layer_rect(
        &gpu.device,
        &gpu.queue,
        slot,
        PixelRect {
            x: 0,
            y: 0,
            width: TILED_W,
            height: TILED_H,
        },
        &bytes,
    );
}

/// Composite into an offscreen target and read one pixel.
///
/// `zoom` above one is what makes a bilinear tap straddle anything at all: at
/// zoom 1 every sample lands on a texel centre and the filter is the identity.
fn tiled_composite(
    gpu: &Gpu,
    canvas: &CanvasRenderer,
    layers: &[LayerDraw],
    zoom: f32,
    center: Vec2,
    x: u32,
    y: u32,
) -> [u8; 4] {
    const VIEW: u32 = 64;
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("tiled-target"),
        size: wgpu::Extent3d {
            width: VIEW,
            height: VIEW,
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
    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    canvas.composite(
        &gpu.queue,
        &mut enc,
        &view,
        &CompositeParams {
            camera: &Camera { center, zoom },
            pivot: Vec2::splat(VIEW as f32 * 0.5),
            layers,
            backdrop: [0.0, 0.0, 0.0],
            // Export, so what comes back is the stack alone: the checkerboard
            // the screen path lays under it would make "is anything here"
            // answerable only by knowing which square this pixel fell in.
            export: true,
            active_index: u32::MAX,
            stroke: StrokeStyle {
                opacity: 0.0,
                ..StrokeStyle::default()
            },
        },
    );
    let row = VIEW * 4;
    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("tiled-readback"),
        size: (row * VIEW) as u64,
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
                rows_per_image: Some(VIEW),
            },
        },
        wgpu::Extent3d {
            width: VIEW,
            height: VIEW,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit(Some(enc.finish()));
    let slice = staging.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
    let mapped = slice.get_mapped_range();
    let at = (y * row + x * 4) as usize;
    let out = [mapped[at], mapped[at + 1], mapped[at + 2], mapped[at + 3]];
    drop(mapped);
    staging.unmap();
    out
}

/// A tile nothing is stored for reads as the slot's **empty value**, and for a
/// layer that is transparent black — byte for byte what a dense slice held
/// there.
///
/// This is the whole of what will make a sparse layer cost what it covers, and
/// today it is reachable only from a test: residency is the identity, so
/// without `unback_tile_for_test` the branch in `tiles.wgsl` would never once
/// have run.
#[test]
fn an_unbacked_layer_tile_reads_as_nothing() {
    let h = harness_or_skip!();
    let gpu = h.gpu;
    let mut canvas = tiled_canvas(gpu, 1);
    fill_tiled_slot(gpu, &mut canvas, 0, [255, 0, 0, 255]);

    let draws = [layer(0, 1.0, BlendMode::Normal)];
    // A twelfth puts the whole 700 × 500 canvas inside the 64 view. Screen pixel
    // `p` is document `(p + 0.5 - 32) * 12 + centre`, so:
    //   x = 8, 32, 46  ->  doc 68, 356, 524  -> tile columns 0, 1, 2
    //   y = 24, 40     ->  doc 160, 352      -> tile rows 0, 1
    let zoom = 1.0 / 12.0;
    let centre = Vec2::new(TILED_W as f32 * 0.5, TILED_H as f32 * 0.5);
    let at =
        |canvas: &CanvasRenderer, x, y| tiled_composite(gpu, canvas, &draws, zoom, centre, x, y);

    for (x, y) in [(8, 24), (32, 24), (46, 24), (8, 40), (32, 40), (46, 40)] {
        assert_eq!(
            at(&canvas, x, y),
            [255, 0, 0, 255],
            "the layer starts opaque everywhere: {x},{y}"
        );
    }

    canvas.unback_tile_for_test(&gpu.queue, 0, (1, 0));
    assert_eq!(
        at(&canvas, 32, 24),
        [0, 0, 0, 0],
        "a tile nothing is stored for is transparent, not whatever was there"
    );
    assert_eq!(
        at(&canvas, 8, 24),
        [255, 0, 0, 255],
        "the tile beside it is untouched"
    );
    assert_eq!(
        at(&canvas, 32, 40),
        [255, 0, 0, 255],
        "and the tile below it is untouched"
    );
    // **Tile (2, 0) is what says the two axes are told apart.** The grid is
    // three by two, so a transposed unpack — in `Entry::cell` or in the WGSL's
    // `tile_atlas_texel` — sends it to (0, 2), which is off the bottom of the
    // page and reads as zero. On a square grid it would land on a tile that
    // exists and holds the same flat colour, and nothing could see it.
    assert_eq!(
        at(&canvas, 46, 24),
        [255, 0, 0, 255],
        "the far column resolves to its own tile, not to a transposed one"
    );
}

/// A tap at the canvas's own border clamps, as the sampler it replaced did.
///
/// `tile_bilinear`'s two `clamp` calls are the whole of `ClampToEdge`, and
/// nothing else reaches them: every other test composites at zoom 1, where the
/// weights are zero and the outer taps are multiplied away, or samples well
/// inside. Delete either clamp and the last half-texel of the right and bottom
/// edges fades into the page's cleared padding at any magnification — which is
/// a soft rim round every document, and exactly the smearing
/// `nothing_outside_a_selections_own_rectangle_is_paintable` refuses on its own
/// terms.
#[test]
fn a_tap_at_the_canvas_edge_clamps_rather_than_reading_the_padding() {
    let h = harness_or_skip!();
    let gpu = h.gpu;
    let mut canvas = tiled_canvas(gpu, 1);
    fill_tiled_slot(gpu, &mut canvas, 0, [255, 0, 0, 255]);

    let draws = [layer(0, 1.0, BlendMode::Normal)];
    // Zoom 4 with the camera on the last column: screen pixel 32's centre is
    // 32.5, so the document coordinate is 699.75 — a quarter of a texel past the
    // centre of the last texel, 699.5. Clamped, both taps are texel 699 and the
    // answer is exactly what is stored. Unclamped, the second tap is texel 700,
    // which is inside the *page* (768 wide) and outside the canvas, so it is
    // cleared — and a quarter of the answer would be nothing.
    let zoom = 4.0;
    let centre = Vec2::new(TILED_W as f32 - 0.25, TILED_H as f32 * 0.5);
    let edge = tiled_composite(gpu, &canvas, &draws, zoom, centre, 32, 32);
    assert_eq!(
        edge,
        [255, 0, 0, 255],
        "a tap past the last texel centre must clamp to it, not fade into the \
         page's margin beyond the canvas"
    );
}

/// A **mask's** empty value is white, not zero.
///
/// A mask multiplies the layer's alpha and a mask nobody has painted on reveals
/// everything, so taking an absent tile for zero hides the layer everywhere
/// nobody painted. That is the same bug `clipstudio.rs` records fixing on the
/// import side, in the same format at the same block size, and it is the one
/// place the substitution is not "whatever a cleared slice held".
#[test]
fn an_unbacked_mask_tile_reveals_the_layer_rather_than_hiding_it() {
    let h = harness_or_skip!();
    let gpu = h.gpu;
    let mut canvas = tiled_canvas(gpu, 2);
    fill_tiled_slot(gpu, &mut canvas, 0, [255, 0, 0, 255]);
    // A mask slice holds coverage on its red channel. Black hides everything.
    fill_tiled_slot(gpu, &mut canvas, 1, [0, 0, 0, 255]);

    let draws = [LayerDraw {
        slot: 0,
        opacity: 1.0,
        blend: BlendMode::Normal.index(),
        visible: true,
        mask: Some(1),
        clipped: false,
    }];
    // The sampling grid `an_unbacked_layer_tile_reads_as_nothing` sets out.
    let zoom = 1.0 / 12.0;
    let centre = Vec2::new(TILED_W as f32 * 0.5, TILED_H as f32 * 0.5);
    assert_eq!(
        tiled_composite(gpu, &canvas, &draws, zoom, centre, 32, 24)[3],
        0,
        "a black mask hides the layer"
    );

    canvas.unback_tile_for_test(&gpu.queue, 1, (1, 0));
    assert_eq!(
        tiled_composite(gpu, &canvas, &draws, zoom, centre, 32, 24),
        [255, 0, 0, 255],
        "where the mask stores nothing the layer is revealed whole"
    );
    assert_eq!(
        tiled_composite(gpu, &canvas, &draws, zoom, centre, 8, 24)[3],
        0,
        "and the tile the mask does store still hides"
    );
}

/// A bilinear tap that straddles a tile boundary blends the **logical**
/// neighbour, not whatever tile happens to sit beside it in the atlas.
///
/// This is the failure the whole design has to answer for. The usual answer is
/// an apron — a copy of the neighbour's edge texels around every tile, refreshed
/// by whoever writes — whose failure mode is a one-texel seam at some zooms on
/// some layers when one writer forgets. `composite.wgsl` reconstructs the tap
/// through the page table instead, so there is nothing to go stale.
///
/// **Under the identity table this is untestable**, because adjacent logical
/// tiles are then adjacent in the atlas too and a shader that ignored the table
/// would give the same answer. `borrow_tile_for_test` is what makes the two
/// answers differ: slot 0's right-hand tile is pointed at slot 1's storage, so a
/// physical read gives red where a resolved one gives blue.
///
/// Demonstrated by mutation: read `c10` and `c11` from `lo` instead of `up` in
/// `tile_bilinear` — a lerp that never crosses a texel — and the straddling
/// pixel comes back pure red.
#[test]
fn a_tap_across_a_tile_boundary_blends_the_logical_neighbour() {
    let h = harness_or_skip!();
    let gpu = h.gpu;
    let mut canvas = tiled_canvas(gpu, 2);
    fill_tiled_slot(gpu, &mut canvas, 0, [255, 0, 0, 255]);
    fill_tiled_slot(gpu, &mut canvas, 1, [0, 0, 255, 255]);
    canvas.borrow_tile_for_test(&gpu.queue, 0, (1, 0), 1);

    let draws = [layer(0, 1.0, BlendMode::Normal)];
    // Zoom 2 with the camera on the boundary: screen pixel 31's centre is 31.5,
    // which is document 255.75 — a quarter of a texel short of the boundary at
    // 256 — so the tap reaches texel 255 (red, tile 0,0) and texel 256 (blue,
    // through the borrow).
    let zoom = 2.0;
    let centre = Vec2::new(256.0, 64.0);

    assert_eq!(
        tiled_composite(gpu, &canvas, &draws, zoom, centre, 16, 32),
        [255, 0, 0, 255],
        "well inside the left tile the tap lands on one texel and is exact"
    );
    assert_eq!(
        tiled_composite(gpu, &canvas, &draws, zoom, centre, 48, 32),
        [0, 0, 255, 255],
        "and inside the borrowed tile it reads what the table points at"
    );

    // The weights are computable exactly: document 255.75, so texel 255 takes
    // 0.75 and texel 256 takes 0.25. Both operands are flat and opaque, so the
    // answer is `0.75 * red + 0.25 * blue` — **in linear light**, because that
    // is where the blend happens, with the encode on the way out. Three quarters
    // of linear one is sRGB 225 and a quarter is 137, not 191 and 64, which is
    // the same trap `composite_pixel`'s own note records: 50% white over black
    // is sRGB 188 rather than 128.
    //
    // Asserted as a value rather than as a threshold, because a threshold
    // survives the two weights being swapped (which here would blend *down* the
    // page, within one tile, and give pure red) or complemented (which would
    // give [137, 0, 225]). Two levels of slack for the store's rounding, which
    // is what the testing rules allow where a tap is not on a texel centre.
    let straddling = tiled_composite(gpu, &canvas, &draws, zoom, centre, 31, 32);
    let near = |got: u8, want: u8| (i32::from(got) - i32::from(want)).abs() <= 2;
    assert!(
        near(straddling[0], 225) && straddling[1] == 0 && near(straddling[2], 137),
        "expected three quarters red and a quarter of the *logical* neighbour, \
         about [225, 0, 137, 255]; got {straddling:?}. Pure red is what reading \
         the physically adjacent tile gives."
    );
    assert_eq!(straddling[3], 255);
}

/// The thumbnail pass substitutes for an unbacked tile too, and it is a
/// *different* shader from the composite.
///
/// `thumbnail.wgsl` and `effect.wgsl` take the same page table and the same
/// prelude, and until this existed only the composite had ever resolved one —
/// so two of the three consumers carried a branch nobody had run. This is the
/// cheap half of that gap; the bake is still uncovered.
#[test]
fn a_thumbnail_reads_an_unbacked_tile_as_nothing_too() {
    let h = harness_or_skip!();
    let gpu = h.gpu;
    let mut canvas = tiled_canvas(gpu, 1);
    fill_tiled_slot(gpu, &mut canvas, 0, [255, 0, 0, 255]);

    let thumb_of = |canvas: &mut CanvasRenderer| -> Thumbnail {
        assert!(canvas.begin_thumb(0));
        // Bounds pass, then picture pass, then the map.
        for _ in 0..2 {
            let mut enc = gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
            canvas.drive_thumb(&gpu.device, &mut enc);
            gpu.queue.submit(Some(enc.finish()));
            canvas.submit_thumb();
            let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
            if let Some(t) = canvas.take_thumb(&gpu.device) {
                return t;
            }
        }
        panic!("the thumbnail never settled");
    };

    let whole = thumb_of(&mut canvas);
    assert!(
        !whole.is_empty(),
        "a layer filled edge to edge is not empty"
    );

    // Take the right-hand half off. The content box is then the left half, so
    // the frame is narrower and the picture is still solid where it draws —
    // what would say the substitution failed is the *bounds* pass having found
    // paint in a tile that stores none.
    canvas.unback_tile_for_test(&gpu.queue, 0, (1, 0));
    canvas.unback_tile_for_test(&gpu.queue, 0, (1, 1));
    canvas.unback_tile_for_test(&gpu.queue, 0, (2, 0));
    canvas.unback_tile_for_test(&gpu.queue, 0, (2, 1));
    let half = thumb_of(&mut canvas);
    assert!(!half.is_empty());
    assert_ne!(
        half.rgba, whole.rgba,
        "half the layer stopped being stored and the thumbnail did not notice"
    );

    // And with nothing stored at all the layer reads as empty, which is the
    // answer a cleared layer will give once `clear_layer` is a table write.
    canvas.unback_tile_for_test(&gpu.queue, 0, (0, 0));
    canvas.unback_tile_for_test(&gpu.queue, 0, (0, 1));
    assert!(
        thumb_of(&mut canvas).is_empty(),
        "a slot storing no tile at all has nothing on it"
    );
}

// ---------------------------------------------------------------------------
// Sparse residency
//
// Stage 2. A tile is stored when something writes to it and not before, so the
// tests below are about three things phase 1 could not have: that residency
// really is sparse, that a cell arriving out of the pool cannot carry the last
// slot's paint with it, and that everything which used to read a canvas-sized
// slice — a readback, a flip, a resize, a capture — still answers with the
// picture.
//
// The fixture keeps `tiled_canvas`'s two properties and needs a third: a slot
// that is *partly* backed beside one that is fully backed, so a bug that
// resolves the wrong slot's table slice has somewhere to show.
// ---------------------------------------------------------------------------

/// A rectangle wholly inside tile `(tx, ty)` of the tiled fixture.
fn in_tile(tx: u32, ty: u32) -> PixelRect {
    PixelRect {
        x: tx * 256 + 8,
        y: ty * 256 + 8,
        width: 16,
        height: 16,
    }
}

fn write_flat(gpu: &Gpu, canvas: &mut CanvasRenderer, slot: u32, rect: PixelRect, rgba: [u8; 4]) {
    let bytes: Vec<u8> = rgba
        .iter()
        .copied()
        .cycle()
        .take((rect.area() * 4) as usize)
        .collect();
    canvas.write_layer_rect(&gpu.device, &gpu.queue, slot, rect, &bytes);
}

/// A layer costs the tiles it covers, and two slots' residencies are their own.
///
/// The headline claim of the whole stage, and it is measured rather than
/// restated: `backed_tiles` counts what the allocator actually handed out.
/// The second slot is what makes a table slice resolved against the wrong slot
/// visible — with one slot painted there is nothing to be confused with.
#[test]
fn a_layer_costs_the_tiles_it_covers_and_no_more() {
    let h = harness_or_skip!();
    let gpu = h.gpu;
    let mut canvas = tiled_canvas(gpu, 2);
    assert_eq!(canvas.backed_tiles(0), 0, "a blank layer stores nothing");
    assert_eq!(canvas.backed_tiles(1), 0);

    write_flat(gpu, &mut canvas, 0, in_tile(0, 0), [255, 0, 0, 255]);
    assert_eq!(
        canvas.backed_tiles(0),
        1,
        "one tile written, one tile stored"
    );
    assert_eq!(canvas.backed_tiles(1), 0, "the other slot paid nothing");

    write_flat(gpu, &mut canvas, 1, in_tile(2, 1), [0, 0, 255, 255]);
    assert_eq!(canvas.backed_tiles(0), 1);
    assert_eq!(canvas.backed_tiles(1), 1);

    // Six tiles between them would be the dense answer. The whole point is that
    // this is two.
    let dense = 3 * 2;
    assert!(
        canvas.backed_tiles(0) + canvas.backed_tiles(1) < dense,
        "residency is not sparse at all"
    );

    // And the pixels are where they were put, in the slot they were put in.
    let read = |canvas: &CanvasRenderer, slot: u32, rect: PixelRect| {
        canvas.read_layer_rect(&gpu.device, &gpu.queue, slot, rect)
    };
    assert_eq!(&read(&canvas, 0, in_tile(0, 0))[..4], &[255, 0, 0, 255]);
    assert_eq!(&read(&canvas, 1, in_tile(2, 1))[..4], &[0, 0, 255, 255]);
    // Slot 1 stores nothing in tile (0, 0) and slot 0 nothing in (2, 1), so
    // both read as the empty value rather than as each other's paint.
    assert_eq!(&read(&canvas, 1, in_tile(0, 0))[..4], &[0, 0, 0, 0]);
    assert_eq!(&read(&canvas, 0, in_tile(2, 1))[..4], &[0, 0, 0, 0]);
}

/// A cell handed back out of the pool arrives at the slot's own empty value.
///
/// **This is the failure that makes an allocator dangerous rather than merely
/// wrong.** An atlas cell is recycled, so it holds whatever the last slot that
/// held it left there; a commit loads and blends, and a partly written tile
/// keeps whatever is around the write. Without the clear in `back_tiles` this
/// reads back another layer's paint — which is not a wrong picture so much as
/// somebody else's picture.
///
/// The fill-then-clear is what makes the cell dirty *and* free, which no
/// ordinary sequence does.
#[test]
fn a_recycled_atlas_cell_carries_none_of_the_last_layers_paint() {
    let h = harness_or_skip!();
    let gpu = h.gpu;
    let mut canvas = tiled_canvas(gpu, 2);

    // Slot 1 takes every cell of the canvas and fills them opaque white.
    fill_tiled_slot(gpu, &mut canvas, 1, [255, 255, 255, 255]);
    assert_eq!(canvas.backed_tiles(1), 6);
    // Then gives them back, still full of white.
    canvas.clear_layer(&gpu.queue, 1);
    assert_eq!(canvas.backed_tiles(1), 0, "a cleared layer stores nothing");

    // Slot 0 writes a small rectangle, which takes one of those cells.
    write_flat(gpu, &mut canvas, 0, in_tile(1, 0), [255, 0, 0, 255]);
    assert_eq!(canvas.backed_tiles(0), 1);

    // Everything in that tile the write did not cover must be transparent, not
    // the white slot 1 left in the cell.
    let whole = PixelRect {
        x: 256,
        y: 0,
        width: 256,
        height: 256,
    };
    let bytes = canvas.read_layer_rect(&gpu.device, &gpu.queue, 0, whole);
    let painted = in_tile(1, 0);
    let mut stray = 0;
    for y in 0..256u32 {
        for x in 0..256u32 {
            let doc_x = 256 + x;
            let inside = doc_x >= painted.x
                && doc_x < painted.x + painted.width
                && y >= painted.y
                && y < painted.y + painted.height;
            if inside {
                continue;
            }
            let at = ((y * 256 + x) * 4) as usize;
            if bytes[at..at + 4] != [0, 0, 0, 0] {
                stray += 1;
            }
        }
    }
    assert_eq!(stray, 0, "a recycled cell was handed over dirty");
}

/// A stroke crossing into a tile nobody has painted backs it before the commit
/// reads it, and the commit lands on both sides of the boundary.
///
/// The dab is centred on the boundary between tiles (0, 0) and (1, 0), so the
/// commit is two draws into two different atlas cells under two different
/// deltas. A delta that was zero — the phase-1 identity — puts the right-hand
/// half of the mark wherever the cell happens to be.
#[test]
fn a_stroke_across_a_tile_boundary_commits_on_both_sides_of_it() {
    let h = harness_or_skip!();
    let gpu = h.gpu;
    let mut canvas = tiled_canvas(gpu, 2);

    // Dirty the pool first, so the cells this stroke takes are recycled ones —
    // the commit's `LoadOp::Load` would otherwise blend over another layer's
    // fill and the colour below would be wrong rather than absent.
    fill_tiled_slot(gpu, &mut canvas, 1, [0, 255, 0, 255]);
    canvas.clear_layer(&gpu.queue, 1);

    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    canvas.begin_frame();
    canvas.draw_dabs(
        &gpu.device,
        &gpu.queue,
        &mut enc,
        &[dab(256.0, 100.0, 20.0, 1.0)],
        DabStyle::default(),
    );
    let damage = PixelRect {
        x: 230,
        y: 74,
        width: 52,
        height: 52,
    };
    canvas.commit_stroke(
        &gpu.device,
        &gpu.queue,
        &mut enc,
        0,
        damage,
        &[damage],
        StrokeStyle {
            color: Color::from_srgb_u8(200, 40, 40, 255),
            opacity: 1.0,
            ..StrokeStyle::default()
        },
    );
    gpu.queue.submit(Some(enc.finish()));
    let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());

    assert_eq!(
        canvas.backed_tiles(0),
        2,
        "a stroke over one boundary reaches two tiles"
    );

    let at = |x: u32, y: u32| {
        let px = canvas.read_layer_rect(
            &gpu.device,
            &gpu.queue,
            0,
            PixelRect {
                x,
                y,
                width: 1,
                height: 1,
            },
        );
        [px[0], px[1], px[2], px[3]]
    };
    // Either side of the boundary, well inside the dab.
    assert!(
        at(246, 100)[3] > 200,
        "the left half of the mark is missing"
    );
    assert!(
        at(266, 100)[3] > 200,
        "the right half of the mark is missing"
    );
    assert_near(at(246, 100), [200, 40, 40], 3, "left of the boundary");
    assert_near(at(266, 100), [200, 40, 40], 3, "right of the boundary");
    // And nothing landed where the dab does not reach — which is what a wrong
    // delta produces.
    assert_eq!(
        at(600, 400)[3],
        0,
        "the mark reached a tile it never touched"
    );
}

/// A new mask costs no storage and still reveals everything.
///
/// `fill_layer_white` is a table write since stage 2: full reveal *is* a mask
/// slot's empty value. What makes this a test of the *store* rather than of the
/// shader is the readback, whose synthesis has to answer white for a mask and
/// transparent for a layer out of the same code.
#[test]
fn a_new_mask_stores_nothing_and_reveals_everything() {
    let h = harness_or_skip!();
    let gpu = h.gpu;
    let mut canvas = tiled_canvas(gpu, 2);
    fill_tiled_slot(gpu, &mut canvas, 0, [255, 0, 0, 255]);

    canvas.fill_layer_white(&gpu.queue, 1);
    assert_eq!(canvas.backed_tiles(1), 0, "an untouched mask costs nothing");

    let read = canvas.read_layer_rect(&gpu.device, &gpu.queue, 1, in_tile(2, 1));
    assert!(
        read.chunks(4).all(|p| p == [255, 255, 255, 255]),
        "a mask nobody has painted on must read as full reveal, not as black"
    );

    // The layer under it is undimmed, which is the whole reason a new mask is
    // white rather than clear.
    let mut masked = layer(0, 1.0, BlendMode::Normal);
    masked.mask = Some(1);
    let centre = Vec2::new(TILED_W as f32 * 0.5, TILED_H as f32 * 0.5);
    let px = tiled_composite(gpu, &canvas, &[masked], 1.0 / 12.0, centre, 32, 32);
    assert_eq!(px[3], 255, "a full-reveal mask hid its layer");

    // Painting on it backs exactly the tiles the stroke reached, and the rest
    // still reveals.
    write_flat(gpu, &mut canvas, 1, in_tile(0, 0), [0, 0, 0, 255]);
    assert_eq!(canvas.backed_tiles(1), 1);
    let still = canvas.read_layer_rect(&gpu.device, &gpu.queue, 1, in_tile(2, 1));
    assert!(
        still.chunks(4).all(|p| p == [255, 255, 255, 255]),
        "painting one tile of a mask changed what the others reveal"
    );
}

/// An undo patch restores a rectangle whose tiles have been given back since.
///
/// The sequence an artist reaches by painting, clearing the layer and undoing:
/// the pieces were captured while the tiles were backed, the clear freed them,
/// and the write-back has to allocate them again rather than write into nothing.
#[test]
fn an_undo_restores_into_a_tile_that_has_been_unbacked_since() {
    let h = harness_or_skip!();
    let gpu = h.gpu;
    let mut canvas = tiled_canvas(gpu, 1);
    let piece = in_tile(1, 1);
    write_flat(gpu, &mut canvas, 0, piece, [10, 200, 30, 255]);

    let before = canvas.read_layer_rect(&gpu.device, &gpu.queue, 0, piece);
    canvas.clear_layer(&gpu.queue, 0);
    assert_eq!(canvas.backed_tiles(0), 0);
    assert!(
        canvas
            .read_layer_rect(&gpu.device, &gpu.queue, 0, piece)
            .chunks(4)
            .all(|p| p == [0, 0, 0, 0]),
        "a cleared layer still had pixels"
    );

    canvas.write_layer_rect(&gpu.device, &gpu.queue, 0, piece, &before);
    let after = canvas.read_layer_rect(&gpu.device, &gpu.queue, 0, piece);
    assert_eq!(after, before, "the undo did not put the pixels back");
    assert_eq!(canvas.backed_tiles(0), 1, "the write backed its own tile");
}

/// A flip mirrors a *sparse* layer exactly, and flipping twice restores it.
///
/// The dense guard runs on the harness's single-tile canvas, where a flip is one
/// pass and one whole-page copy. This is the case that pass exists for: the
/// destination tile at the left edge is made of the right edge's texels, which
/// on a canvas that is not a whole number of tiles straddles two source tiles —
/// so a flip is the one storage move that cannot be a translation.
#[test]
fn a_flip_mirrors_a_sparse_layer_and_flipping_twice_restores_it_exactly() {
    let h = harness_or_skip!();
    let gpu = h.gpu;
    let mut canvas = tiled_canvas(gpu, 2);

    // Two marks, in tiles that mirror onto different tiles. 700 wide is 2.73
    // tiles, so the mirror of tile 0 lands across tiles 1 and 2.
    let left = in_tile(0, 0);
    let right = in_tile(2, 1);
    write_flat(gpu, &mut canvas, 0, left, [200, 40, 40, 255]);
    write_flat(gpu, &mut canvas, 0, right, [40, 40, 200, 255]);
    let backed = canvas.backed_tiles(0);
    assert_eq!(backed, 2);

    let read_all = |canvas: &CanvasRenderer| {
        canvas.read_layer_rect(
            &gpu.device,
            &gpu.queue,
            0,
            PixelRect {
                x: 0,
                y: 0,
                width: TILED_W,
                height: TILED_H,
            },
        )
    };
    let before = read_all(&canvas);

    canvas.flip_layers(&gpu.device, &gpu.queue, &[0], FlipAxis::Horizontal);
    let once = read_all(&canvas);
    assert_ne!(once, before, "the flip did nothing");

    // The left mark is now on the right, exactly mirrored.
    let px = |bytes: &[u8], x: u32, y: u32| {
        let at = ((y * TILED_W + x) * 4) as usize;
        [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]
    };
    let mirror_x = TILED_W - 1 - (left.x + 4);
    assert_eq!(
        px(&once, mirror_x, left.y + 4),
        [200, 40, 40, 255],
        "the mark did not arrive at its mirror position"
    );
    assert_eq!(
        px(&once, left.x + 4, left.y + 4),
        [0, 0, 0, 0],
        "the mark is still where it was as well"
    );

    canvas.flip_layers(&gpu.device, &gpu.queue, &[0], FlipAxis::Horizontal);
    assert_eq!(
        read_all(&canvas),
        before,
        "flipping twice is not the identity, so undoing a flip loses a level"
    );
    // **A flip coarsens residency and this is where that is said out loud.**
    // Which destination tiles a flip has to store is derived from which *source
    // tiles* hold something, and a 256-wide source tile mirrored onto a canvas
    // that is not a whole number of tiles lands across two destination tiles —
    // so a tile that held one mark becomes two that hold half of one each. The
    // picture is exact, which is what the assertion above says; the storage is
    // an over-approximation that grows towards dense under repeated flips and
    // is bounded by the grid. Nothing short of knowing where the paint is
    // *inside* a tile can do better, and that is a readback.
    // `<= 3 * 2` was here first and said nothing: the fixture's grid *is* 3×2,
    // so it could only fail on a corrupt table. What the coarsening actually has
    // to be is **bounded away from dense** — two marks in two tiles must not
    // have taken the whole layer — which is the claim a reader would want and
    // the one a wider mirror would break.
    assert!(canvas.backed_tiles(0) >= backed);
    assert!(
        canvas.backed_tiles(0) < 3 * 2,
        "two flips of two marks took the whole grid: {} tiles",
        canvas.backed_tiles(0)
    );
}

/// A resize carries a sparse layer, and a destination tile it only partly fills
/// is the slot's empty value everywhere else.
///
/// Every move a resize makes is a translation, so this is copies alone — no
/// scratch and no pass. The trap a scratch would have carried is exactly the
/// second assertion: a shifted destination tile reaches outside what any source
/// tile covers.
#[test]
fn a_resize_carries_a_sparse_layer_and_leaves_no_stale_texels() {
    let h = harness_or_skip!();
    let gpu = h.gpu;
    let mut canvas = tiled_canvas(gpu, 2);
    // Dirty the pool, so a destination cell that is not cleared shows it.
    fill_tiled_slot(gpu, &mut canvas, 1, [0, 255, 0, 255]);
    canvas.clear_layer(&gpu.queue, 1);

    let mark = in_tile(1, 0);
    write_flat(gpu, &mut canvas, 0, mark, [200, 40, 40, 255]);

    // Grow, anchored top-left, so the picture does not move and the arithmetic
    // is checkable by hand.
    canvas.resize(
        &gpu.device,
        &gpu.queue,
        UVec2::new(900, 700),
        Anchor::TopLeft,
        2,
    );

    let bytes = canvas.read_layer_rect(
        &gpu.device,
        &gpu.queue,
        0,
        PixelRect {
            x: 0,
            y: 0,
            width: 900,
            height: 700,
        },
    );
    let px = |x: u32, y: u32| {
        let at = ((y * 900 + x) * 4) as usize;
        [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]
    };
    assert_eq!(
        px(mark.x + 4, mark.y + 4),
        [200, 40, 40, 255],
        "the mark did not survive the resize"
    );
    // Everything else, including the region the new canvas added and the rest of
    // the destination tile, is the empty value rather than the green the pool
    // was dirtied with.
    let mut stray = 0;
    for y in 0..700u32 {
        for x in 0..900u32 {
            let inside =
                x >= mark.x && x < mark.x + mark.width && y >= mark.y && y < mark.y + mark.height;
            if inside {
                continue;
            }
            if px(x, y) != [0, 0, 0, 0] {
                stray += 1;
            }
        }
    }
    assert_eq!(
        stray, 0,
        "a resized layer came back with texels nobody wrote"
    );
}

/// A blended commit on a tiled layer reads its backdrop out of the right cell.
///
/// Multiply against white is the identity and against black is black, which are
/// exact whatever the rounding — so this says the backdrop copy found the
/// layer's own texels rather than another cell's. The two tiles carry different
/// backdrops on purpose: one answer that happened to be right for both would say
/// nothing.
#[test]
fn a_blended_commit_finds_its_backdrop_in_the_right_tile() {
    let h = harness_or_skip!();
    let gpu = h.gpu;
    let mut canvas = tiled_canvas(gpu, 1);
    write_flat(gpu, &mut canvas, 0, in_tile(0, 0), [255, 255, 255, 255]);
    write_flat(gpu, &mut canvas, 0, in_tile(1, 0), [0, 0, 0, 255]);

    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    canvas.begin_frame();
    canvas.draw_dabs(
        &gpu.device,
        &gpu.queue,
        &mut enc,
        &[
            dab(
                in_tile(0, 0).x as f32 + 8.0,
                in_tile(0, 0).y as f32 + 8.0,
                6.0,
                1.0,
            ),
            dab(
                in_tile(1, 0).x as f32 + 8.0,
                in_tile(1, 0).y as f32 + 8.0,
                6.0,
                1.0,
            ),
        ],
        DabStyle::default(),
    );
    let pieces = [in_tile(0, 0), in_tile(1, 0)];
    let span = PixelRect {
        x: pieces[0].x,
        y: pieces[0].y,
        width: pieces[1].x + pieces[1].width - pieces[0].x,
        height: 16,
    };
    canvas.commit_stroke(
        &gpu.device,
        &gpu.queue,
        &mut enc,
        0,
        span,
        &pieces,
        StrokeStyle {
            color: Color::from_srgb_u8(255, 255, 255, 255),
            opacity: 1.0,
            blend: BlendMode::Multiply,
            ..StrokeStyle::default()
        },
    );
    gpu.queue.submit(Some(enc.finish()));
    let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());

    let at = |x: u32, y: u32| {
        let px = canvas.read_layer_rect(
            &gpu.device,
            &gpu.queue,
            0,
            PixelRect {
                x,
                y,
                width: 1,
                height: 1,
            },
        );
        [px[0], px[1], px[2], px[3]]
    };
    // White multiplied by white is white; black multiplied by white is black.
    let (a, b) = (in_tile(0, 0), in_tile(1, 0));
    assert_near(
        at(a.x + 8, a.y + 8),
        [255, 255, 255],
        2,
        "multiply over white",
    );
    assert_near(at(b.x + 8, b.y + 8), [0, 0, 0], 2, "multiply over black");
}

/// A growth part-way through an encoder does not lose what was already recorded
/// into it.
///
/// **The one defect in this stage that no other guard could see.** A growth
/// replaces the atlas texture and copies the old one into the new; recorded on
/// its own encoder and submitted, that copy reads the old texture *before* the
/// caller's still-open encoder writes to it, so everything already recorded
/// there lands in a texture nothing will ever read again. In `render` that is
/// the float's preview, drawn into the frame's encoder several statements before
/// an effect bake can promote a slot and grow the atlas — a preview that freezes
/// for a frame, on the frame a document gets one layer heavier.
///
/// The sequence here is the same shape and needs no float: two commits into one
/// encoder, with the pool arranged so the *second* is what runs out of cells.
#[test]
fn a_growth_part_way_through_an_encoder_keeps_what_was_recorded_before_it() {
    let h = harness_or_skip!();
    let gpu = h.gpu;
    let mut canvas = tiled_canvas(gpu, 1);
    let pages_before = canvas.page_count();

    // Fill the pool down to **exactly one free cell**, so the first commit
    // below fits and the second cannot. Six tiles a slot, so this walks slots
    // rather than restating the fixture arithmetic.
    let grid = [(0u32, 0u32), (1, 0), (2, 0), (0, 1), (1, 1), (2, 1)];
    // Derived rather than looped-until-satisfied: an unbounded `while` here
    // walks `spare` past `MAX_SLOTS` the day the starting atlas gets bigger,
    // `write_flat` logs and returns, `free_tiles` stops moving and the test
    // **hangs** rather than failing. The count is what it is: whole slots first,
    // then the remainder one tile at a time, leaving exactly one cell.
    let cells = canvas.free_tiles();
    assert_eq!(cells, canvas.page_count() as usize * grid.len());
    let full_slots = (cells - 1) / grid.len();
    let remainder = (cells - 1) % grid.len();
    assert!(
        3 + full_slots < 256,
        "the fixture outgrew the slot ceiling: {full_slots} slots wanted"
    );
    for spare in 2..2 + full_slots as u32 {
        fill_tiled_slot(gpu, &mut canvas, spare, [0, 0, 255, 255]);
    }
    let resident = 2 + full_slots as u32 - 1;
    let spare = 2 + full_slots as u32;
    for cell in grid.iter().take(remainder) {
        write_flat(
            gpu,
            &mut canvas,
            spare,
            in_tile(cell.0, cell.1),
            [0, 0, 255, 255],
        );
    }
    assert_eq!(canvas.free_tiles(), 1, "exactly one cell left");
    assert_eq!(canvas.page_count(), pages_before, "nothing has grown yet");

    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });

    // First commit: into tile (2, 1), which takes the last free cell. Recorded
    // against the atlas as it stands.
    let first = in_tile(2, 1);
    canvas.begin_frame();
    canvas.draw_dabs(
        &gpu.device,
        &gpu.queue,
        &mut enc,
        &[dab(first.x as f32 + 8.0, first.y as f32 + 8.0, 10.0, 1.0)],
        DabStyle::default(),
    );
    canvas.commit_stroke(
        &gpu.device,
        &gpu.queue,
        &mut enc,
        0,
        first,
        &[first],
        StrokeStyle {
            color: Color::from_srgb_u8(200, 40, 40, 255),
            opacity: 1.0,
            ..StrokeStyle::default()
        },
    );
    assert_eq!(canvas.free_tiles(), 0, "the pool should now be empty");
    assert_eq!(canvas.page_count(), pages_before, "nothing has grown yet");

    // Second commit: a different slot and a different tile, so it needs a cell
    // the pool does not have. This is the growth.
    let second = in_tile(0, 0);
    canvas.draw_dabs(
        &gpu.device,
        &gpu.queue,
        &mut enc,
        &[dab(second.x as f32 + 8.0, second.y as f32 + 8.0, 10.0, 1.0)],
        DabStyle::default(),
    );
    canvas.commit_stroke(
        &gpu.device,
        &gpu.queue,
        &mut enc,
        1,
        second,
        &[second],
        StrokeStyle {
            color: Color::from_srgb_u8(40, 200, 40, 255),
            opacity: 1.0,
            ..StrokeStyle::default()
        },
    );
    assert!(
        canvas.page_count() > pages_before,
        "the atlas should have grown"
    );

    gpu.queue.submit(Some(enc.finish()));
    let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());

    let at = |slot: u32, x: u32, y: u32| {
        let px = canvas.read_layer_rect(
            &gpu.device,
            &gpu.queue,
            slot,
            PixelRect {
                x,
                y,
                width: 1,
                height: 1,
            },
        );
        [px[0], px[1], px[2], px[3]]
    };
    // The first commit was recorded before the growth. It has to have survived
    // it — this is the assertion the whole test is for.
    assert_near(
        at(0, first.x + 8, first.y + 8),
        [200, 40, 40],
        3,
        "the commit recorded before the growth was lost",
    );
    // And the second, which was recorded after.
    assert_near(
        at(1, second.x + 8, second.y + 8),
        [40, 200, 40],
        3,
        "the commit that caused the growth was lost",
    );
    // And so did the layer that was merely sitting there.
    assert_eq!(
        at(resident, 100, 100),
        [0, 0, 255, 255],
        "a resident layer was lost"
    );
}

/// Every atlas cell is held by exactly one slot or is free, through a session
/// that exercises every path which moves one.
///
/// **The property whose failure is silent and total**, which is why it is
/// checked as a set rather than looked for in a pixel: a cell issued twice is
/// one layer's paint appearing in another's, and a cell leaked is storage
/// nothing can ever take back. Neither shows up until the two slots happen to be
/// drawn together.
///
/// It is checked after *every* step rather than at the end, because the step
/// that broke it is the whole of what a failure has to say.
#[test]
fn every_atlas_cell_is_held_by_one_slot_or_is_free() {
    let h = harness_or_skip!();
    let gpu = h.gpu;
    let mut canvas = tiled_canvas(gpu, 2);
    let check = |canvas: &CanvasRenderer, step: &str| {
        if let Err(e) = canvas.atlas_invariant() {
            panic!("after {step}: {e}");
        }
    };
    check(&canvas, "a fresh store");

    write_flat(gpu, &mut canvas, 0, in_tile(0, 0), [255, 0, 0, 255]);
    write_flat(gpu, &mut canvas, 0, in_tile(2, 1), [255, 0, 0, 255]);
    check(&canvas, "two writes");

    fill_tiled_slot(gpu, &mut canvas, 1, [0, 255, 0, 255]);
    check(&canvas, "a full slot");

    canvas.fill_layer_white(&gpu.queue, 3);
    check(&canvas, "a new mask");
    write_flat(gpu, &mut canvas, 3, in_tile(1, 1), [0, 0, 0, 255]);
    check(&canvas, "painting on the mask");

    canvas.clear_layer(&gpu.queue, 1);
    check(&canvas, "clearing the full slot");

    // A commit, which backs through a different door.
    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    canvas.begin_frame();
    canvas.draw_dabs(
        &gpu.device,
        &gpu.queue,
        &mut enc,
        &[dab(256.0, 300.0, 24.0, 1.0)],
        DabStyle::default(),
    );
    let damage = PixelRect {
        x: 224,
        y: 268,
        width: 64,
        height: 64,
    };
    canvas.commit_stroke(
        &gpu.device,
        &gpu.queue,
        &mut enc,
        0,
        damage,
        &[damage],
        StrokeStyle {
            color: Color::WHITE,
            opacity: 1.0,
            ..StrokeStyle::default()
        },
    );
    gpu.queue.submit(Some(enc.finish()));
    check(&canvas, "a commit across a boundary");

    // A lift, which is the one path that **promotes** a slot that already holds
    // tiles — the cells it moves out of have to go back to the pool, and a
    // promotion that leaked them would be invisible to every pixel assertion.
    let lift = in_tile(0, 0);
    let preview = canvas
        .begin_float(
            &gpu.device,
            &gpu.queue,
            5,
            &FloatSource {
                slot: 0,
                rect: lift,
                pixels: None,
                mask: None,
            },
        )
        .expect("no room for a preview");
    check(&canvas, "a lift");
    assert!(canvas.backed_tiles(0) >= 6, "the lifted slot owns a page");
    assert!(canvas.backed_tiles(preview) >= 6, "the preview owns a page");
    canvas.end_float(&gpu.queue);
    check(&canvas, "putting the float down");
    assert_eq!(
        canvas.backed_tiles(preview),
        0,
        "the preview kept its page after the float ended"
    );

    canvas.flip_layers(&gpu.device, &gpu.queue, &[0, 3], FlipAxis::Horizontal);
    check(&canvas, "a horizontal flip");
    canvas.flip_layers(&gpu.device, &gpu.queue, &[0, 3], FlipAxis::Vertical);
    check(&canvas, "a vertical flip");

    canvas.resize(
        &gpu.device,
        &gpu.queue,
        UVec2::new(900, 400),
        Anchor::Centre,
        4,
    );
    check(&canvas, "a shrinking resize");

    canvas.clear_all_layers(&gpu.queue);
    check(&canvas, "clearing everything");
    assert_eq!(
        canvas.free_tiles(),
        canvas.page_count() as usize * 4 * 2,
        "clearing everything did not give every cell back"
    );
}

/// A capture of a **partly-painted mask** reads what the blocking path reads.
///
/// **This is the defect a critic found and the sentence that hid it.** The
/// capture used to fill its band with `clear_buffer`'s zeroes, under a comment
/// asserting that a partly-backed mask could not arise — "every mask a save or
/// an autosave reads is fully backed, from an import's single canvas piece or
/// from a stroke". That stopped being true in the same change that wrote it:
/// `fill_layer_white` became a table write, so a mask stores nothing until
/// somebody paints on it, and *add a mask, paint on part of it* is the ordinary
/// workflow. Zeroes on `.r` are coverage 0, so the autosaved file hid the layer
/// everywhere the artist had not painted on its mask — while the explicit Save,
/// which goes through `read_layer_pieces` and synthesises properly, did not.
/// Two writers of one document disagreeing.
///
/// Driven on the tiled fixture, because the harness's 64-square canvas is one
/// tile and a mask there is either wholly backed or wholly absent.
#[test]
fn a_capture_of_a_partly_painted_mask_reveals_what_the_save_does() {
    let h = harness_or_skip!();
    let gpu = h.gpu;
    let mut canvas = tiled_canvas(gpu, 2);
    fill_tiled_slot(gpu, &mut canvas, 0, [200, 40, 40, 255]);

    // A mask, painted on in one tile of six. This is the state the old comment
    // said was unreachable.
    canvas.fill_layer_white(&gpu.queue, 1);
    write_flat(gpu, &mut canvas, 1, in_tile(0, 0), [0, 0, 0, 255]);
    assert_eq!(canvas.backed_tiles(1), 1, "the mask is partly stored");
    assert!(
        canvas.backed_tiles(1) < 6,
        "the fixture stopped being partly backed"
    );

    let draws = vec![LayerDraw {
        slot: 0,
        opacity: 1.0,
        blend: 0,
        visible: true,
        mask: Some(1),
        clipped: false,
    }];
    let full = PixelRect {
        x: 0,
        y: 0,
        width: TILED_W,
        height: TILED_H,
    };
    let expected: Vec<Vec<u8>> = (0..2)
        .map(|slot| canvas.read_layer_rect(&gpu.device, &gpu.queue, slot, full))
        .collect();
    // The blocking path is the reference, and it has to be right for the
    // *mask*: full reveal is `[255; 4]`, not zeroes.
    assert!(
        expected[1]
            .chunks(4)
            .filter(|p| *p == [255, 255, 255, 255])
            .count()
            > 0,
        "the reference itself reports no revealed pixel"
    );

    let (captured, _, _) = run_capture(gpu, &mut canvas, &[0, 1], &draws);
    assert_eq!(captured.layers.len(), 2);
    assert_eq!(
        captured.layers[0], expected[0],
        "the layer came back differently"
    );
    assert_eq!(
        captured.layers[1], expected[1],
        "an autosaved mask hides what a saved one reveals"
    );

    // **And the flattened preview, which is the step after the last layer.**
    // `gaps` and `empty` describe the *step*, so a merged image that inherited
    // the previous one's would come back with holes shaped like wherever that
    // layer happened not to be stored — `mergedimage.png` punched out, in a
    // file nothing else in the suite reads.
    let expected_merged = canvas.export_rgba(&gpu.device, &gpu.queue, &draws);
    assert_eq!(
        captured.merged, expected_merged,
        "the flattened preview came back with holes"
    );
}

/// An effect that has been switched off hands its **page** back.
///
/// **A defect a critic found, and the comment that hid it.** An effect slice is
/// page-backed — its pixels are a whole canvas by construction — and the code
/// beside `promote` claimed the page came back through `EffectCache::forget_all`
/// and `retain_only`. Neither could: that type holds no `LayerStore` and cannot
/// reach the pool. So a drop shadow enabled and then removed left its page
/// `Owned` by a slot nothing named, for the life of the document — 395 MB on the
/// 20000×5000 file, per effect, until a resize.
///
/// Measured on the pool rather than on a pixel, because a held page is invisible
/// in the picture: that is the whole reason it went unnoticed.
#[test]
fn an_effect_switched_off_gives_its_page_back() {
    let mut h = harness_or_skip!();
    h.fill(0, Color::WHITE);
    let plain = layer(0, 1.0, BlendMode::Normal);
    let frame = EffectFrame {
        active_index: u32::MAX,
        stroke: StrokeStyle::default(),
        stroke_live: false,
    };

    let free_before = h.canvas.free_tiles();
    let cast = [shadow(Color::BLACK, 180.0, 12.0)];
    let baked = h.bake_frame(&[effected(plain, &cast)], 1, frame);
    assert_eq!(baked.draws.len(), 2, "the shadow produced no draw");
    assert!(
        h.canvas.free_tiles() < free_before,
        "the effect took no page at all, so this proves nothing"
    );

    // Switched off: the same stack with no effects on it. This is the route
    // through `release_effect_pages`.
    h.bake_frame(&[effected(plain, &[])], 1, frame);
    assert_eq!(
        h.canvas.free_tiles(),
        free_before,
        "the effect's page was never handed back"
    );
    assert!(h.canvas.atlas_invariant().is_ok());

    // **And the other route, which is a different method.** Two effects, then
    // one: the entry that is dropped goes through `retain_only`, and a guard
    // that drove only the empty case left that one silent — demonstrated by
    // mutation, which is how this second half came to be here.
    let both = [
        shadow(Color::BLACK, 180.0, 12.0),
        outline(Color::BLACK, 4.0, OutlinePosition::Outside),
    ];
    let baked = h.bake_frame(&[effected(plain, &both)], 1, frame);
    assert_eq!(baked.draws.len(), 3, "two effects produced no two draws");
    let free_with_two = h.canvas.free_tiles();
    let baked = h.bake_frame(&[effected(plain, &both[..1])], 1, frame);
    assert_eq!(baked.draws.len(), 2, "the second effect is still drawn");
    assert!(
        h.canvas.free_tiles() > free_with_two,
        "dropping one effect of two handed no page back"
    );
    assert!(h.canvas.atlas_invariant().is_ok());
}
