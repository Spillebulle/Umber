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
    BlendMode, Brush, BrushMode, Camera, Color, Dab, InputPoint, PixelRect, StrokeBuilder, TipMask,
};
use umber_render::{
    CanvasRenderer, CompositeParams, DabStyle, Gpu, LayerDraw, ProbeParams, StrokeStyle,
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
    canvas.commit_stroke(
        &gpu.queue,
        &mut enc,
        0,
        PixelRect {
            x: 0,
            y: 0,
            width: DOC,
            height: DOC,
        },
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
