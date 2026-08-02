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
    InputPoint, Modulation, PixelRect, Rect, ResponseCurve, StrokeBuilder, TileMask, TipMask,
};
use umber_render::{
    CanvasRenderer, CompositeParams, DabStyle, DocumentCapture, Gpu, LayerDraw, ProbeParams,
    StrokeStyle,
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
        pollster::block_on(Gpu::new(instance, None)).ok()
    })
    .as_ref()
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
        let canvas = CanvasRenderer::new(&gpu.device, UVec2::new(DOC, DOC), TARGET_FORMAT);

        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        canvas.clear_all_layers(&mut enc);
        canvas.clear_stroke(&mut enc);
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
        // The whole rect as one piece: these tests commit everything they
        // stamped. `a_stroke_only_touches_the_cells_its_dabs_reached` is the
        // one that exercises a patch cut to a damage mask.
        self.canvas.commit_stroke(
            &self.gpu.queue,
            &mut enc,
            slot,
            rect,
            &[rect],
            StrokeStyle {
                color,
                opacity,
                mode,
                per_dab_color: self.per_dab_color,
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
    fn set_tip(&mut self, tip: Option<TipMask>) {
        self.canvas
            .set_tip(&self.gpu.device, &self.gpu.queue, tip.map(Arc::new));
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
                pivot: Vec2::splat(DOC as f32 * 0.5),
                layers,
                backdrop: [0.0, 0.0, 0.0],
                export: false,
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
    // quantisation and not a leak in the formula. Sixteen-bit coverage would
    // close it, at four times the bandwidth of the hottest texture in the
    // frame, to remove a difference of 0.4%.
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

    let mut enc = h.encoder();
    h.canvas.clear_all_layers(&mut enc);
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
    let h = harness_or_skip!();
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
        .write_layer_rect(&h.gpu.queue, beyond, rect, &bytes);
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
        UVec2::new(DOC, DOC),
        wgpu::TextureFormat::Bgra8Unorm,
    );

    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    canvas.clear_all_layers(&mut enc);
    canvas.clear_stroke(&mut enc);
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
    let whole = PixelRect {
        x: 0,
        y: 0,
        width: DOC,
        height: DOC,
    };
    canvas.commit_stroke(
        &gpu.queue,
        &mut enc,
        0,
        whole,
        &[whole],
        StrokeStyle {
            color: Color::from_srgb_u8(200, 20, 20, 255),
            opacity: 1.0,
            mode: BrushMode::Paint,
            per_dab_color: false,
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
    // Generous: a few frames per step, and a step is one layer. A capture that
    // has not finished inside this is a bug, not a slow machine.
    for _ in 0..2000 {
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
    panic!("the capture never came home");
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
        },
        LayerDraw {
            slot: 1,
            opacity: 1.0,
            blend: 0,
            visible: true,
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
        },
        LayerDraw {
            slot: 1,
            opacity: 1.0,
            blend: 0,
            visible: true,
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
    );
    h.canvas.ensure_slots(&h.gpu.device, &h.gpu.queue, LAYERS);
    let mut enc = h.encoder();
    h.canvas.clear_all_layers(&mut enc);
    h.gpu.queue.submit(Some(enc.finish()));

    let slots: Vec<u32> = (0..LAYERS).collect();
    let draws: Vec<LayerDraw> = slots
        .iter()
        .map(|slot| LayerDraw {
            slot: *slot,
            opacity: 1.0,
            blend: 0,
            visible: true,
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

// --- patches cut to the cells a stroke reached -----------------------------

/// A canvas large enough to hold several damage cells, with a layer full of
/// pixels no two of which are alike.
///
/// Random, because the point of every test below is that certain bytes come
/// back *exactly*: a flat layer would pass them all while restoring nothing.
fn noisy_canvas(gpu: &Gpu, side: u32) -> CanvasRenderer {
    let canvas = CanvasRenderer::new(&gpu.device, UVec2::splat(side), TARGET_FORMAT);
    let mut enc = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    canvas.clear_all_layers(&mut enc);
    canvas.clear_stroke(&mut enc);
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
    canvas.write_layer_rect(&gpu.queue, 0, whole_of(side), &pixels);
    canvas
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
        &gpu.queue,
        &mut enc,
        0,
        rect,
        &pieces,
        StrokeStyle {
            color: Color::from_srgb_u8(200, 20, 20, 255),
            opacity: 1.0,
            mode: BrushMode::Paint,
            per_dab_color: false,
        },
    );
    gpu.queue.submit(Some(enc.finish()));

    let painted = canvas.read_layer_rect(&gpu.device, &gpu.queue, 0, whole_of(SIDE));
    assert_ne!(painted, before, "the stroke painted nothing");

    for (piece, bytes) in pieces.iter().zip(&patch) {
        canvas.write_layer_rect(&gpu.queue, 0, *piece, bytes);
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
