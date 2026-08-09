//! The startup splash, painted before the GPU exists.
//!
//! Timing `UmberApp::resumed` stage by stage is what decided the shape of this
//! module. On the development machine (Windows, D3D12):
//!
//! | stage | cost |
//! |---|---|
//! | creating the window | 50–250 ms |
//! | `Gpu::new` — adapter request and device creation | 0.7–5 s |
//! | configuring the surface | 0.5–0.75 s |
//! | `CanvasRenderer::new` — pipelines and shaders | 10–90 ms |
//! | egui context and Archivo | under 10 ms |
//!
//! The window is created early and then sits **blank** for the whole of the
//! rest. That is the gap, and it is not small. It is also unreachable from
//! anything drawn on the GPU: a wgpu-drawn splash could not appear until the
//! very thing it would be covering had already finished.
//!
//! So this paints the window with no GPU at all. `softbuffer` hands over a plain
//! CPU framebuffer for a winit window, the mark comes from `logo.rs`'s distance
//! field, and the text from `cputext.rs` — which also means the wordmark gets
//! Archivo at the weight 900 the design asks for, rather than the Regular master
//! egui is stuck with. The whole splash is one `Vec<u32>` written by [`render`],
//! which is a pure function of size, stage and adapter name, and therefore
//! something a test can rasterise to a PNG and look at.
//!
//! ### What it will not do
//!
//! * **No sleeping, and no hold.** Each stage is painted immediately *before*
//!   the work it names begins, and the splash is dropped the moment `resumed`
//!   returns. It cannot delay startup by more than the few milliseconds its own
//!   four repaints cost, and it vanishes the instant the canvas is ready no
//!   matter where the bar has reached.
//! * **No invented progress.** The bar advances only at real stage boundaries,
//!   and because each stage is painted before its work runs, the bar is always
//!   *behind* the truth and never ahead of it. Nothing animates in between,
//!   because nothing inside `Gpu::new` reports sub-progress and a bar that crept
//!   forward on a timer would be inventing it.
//!
//! ### What that costs, honestly
//!
//! During the long adapter stall the splash is **static**. The main thread is
//! blocked inside `Gpu::new`, exactly as it was before this module existed, so
//! there is no repaint and no pulse on the status text as the design shows. The
//! status line does say what is happening, which was the point of it. Animating
//! through the stall needs `Gpu::new` moved to a worker thread and the main
//! thread pumping frames while it waits — worth doing, but a restructure of
//! `resumed` rather than an addition to it.
//!
//! The design's "click anywhere to skip" is likewise not implemented, and
//! deliberately: the splash's lifetime *is* the startup work, so there is
//! nothing left to skip to. The only way to make a skip meaningful would be to
//! keep the splash up after the canvas was ready, which is precisely the lie
//! about latency this application exists not to tell.

use crate::cputext::Font;
use crate::logo;
use crate::theme::Palette;
use crate::theme::contrast::{self, Ink};
use egui::Color32;
use std::num::NonZeroU32;
use std::sync::Arc;
use winit::window::Window;

/// The startup steps the splash can name.
///
/// Each is painted *before* the work it describes runs, so the status line
/// always names what Umber is about to do, and the bar always understates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// `Gpu::new` — by far the longest, and the one the splash exists for.
    Adapter,
    Surface,
    Shaders,
    Fonts,
    Ready,
}

impl Stage {
    /// How far along the bar sits when this stage begins.
    ///
    /// These are time shares taken from the measurements in the module comment,
    /// not equal steps. Four evenly spaced stages would put the bar at a quarter
    /// for four fifths of the wall clock, which looks stuck; weighting them by
    /// how long each actually takes makes the bar move roughly linearly in time.
    /// They are approximations of a figure that varies by machine, which is all
    /// a progress bar can ever be, but they are approximations of something
    /// measured.
    fn progress(self) -> f32 {
        match self {
            Self::Adapter => 0.04,
            Self::Surface => 0.70,
            Self::Shaders => 0.90,
            Self::Fonts => 0.96,
            Self::Ready => 1.0,
        }
    }

    /// The design writes this line as `compiling shaders — dab.wgsl`; the others
    /// follow its lower-case, present-participle voice.
    fn status(self) -> &'static str {
        match self {
            Self::Adapter => "requesting a GPU adapter",
            Self::Surface => "configuring the surface",
            Self::Shaders => "compiling shaders: dab.wgsl",
            Self::Fonts => "loading Archivo",
            Self::Ready => "ready",
        }
    }
}

/// Sizes straight from the design, in logical points, scaled by the window's
/// DPI factor at paint time.
///
/// Not in `theme::metrics` because the splash is a screen of its own rather than
/// part of the workspace those tokens describe.
mod design {
    /// The mark beside the wordmark.
    pub const MARK: f32 = 52.0;
    /// Gap between the mark and the wordmark.
    pub const MARK_GAP: f32 = 18.0;
    pub const WORDMARK: f32 = 64.0;
    /// The design tracks the wordmark tight.
    pub const WORDMARK_TRACKING: f32 = -2.0;
    /// Archivo Black. `cputext` can actually deliver this; egui cannot.
    pub const WORDMARK_WEIGHT: f32 = 900.0;
    pub const TAGLINE: f32 = 11.5;
    pub const TAGLINE_TRACKING: f32 = 3.0;
    /// Gap between the wordmark row and the tagline.
    pub const TAGLINE_GAP: f32 = 10.0;
    pub const STATUS: f32 = 10.5;
    pub const BAR_HEIGHT: f32 = 3.0;
    pub const BAR_RADIUS: f32 = 2.0;
    /// Gap between the bar and the status line under it.
    pub const BAR_GAP: f32 = 10.0;
    /// The progress block is inset by this fraction of the width on each side,
    /// and sits this fraction of the height up from the bottom.
    pub const BLOCK_INSET: f32 = 0.20;
    pub const BLOCK_BOTTOM: f32 = 0.18;
}

/// The tagline under the wordmark. The middle dot is in Archivo — there is a
/// test that says so, because the interface's rule against Unicode symbols
/// exists for the ones that are not.
const TAGLINE: &str = concat!("GPU PAINTING · v", env!("CARGO_PKG_VERSION"));

pub struct Splash {
    /// `None` when softbuffer could not attach — a headless session, an
    /// unsupported platform, a driver refusing the surface. Every method then
    /// becomes a no-op, because failing to draw a logo must never stop Umber
    /// from starting.
    surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    /// Kept alive alongside the surface; softbuffer's platform backends hold
    /// shared state on it.
    _context: Option<softbuffer::Context<Arc<Window>>>,
    window: Arc<Window>,
    palette: Palette,
    /// Backend and device, once `Gpu::new` has told us. Empty before that, which
    /// is unavoidable: the adapter is not known until the end of the stage that
    /// finding it is the whole cost of.
    adapter: String,
}

impl Splash {
    /// Attach to a freshly created window.
    ///
    /// Never fails: if softbuffer will not attach, the splash simply does
    /// nothing from here on.
    pub fn new(window: Arc<Window>, palette: Palette) -> Self {
        let context = softbuffer::Context::new(window.clone())
            .inspect_err(|e| log::debug!("no software surface for the splash: {e}"))
            .ok();
        let surface = context
            .as_ref()
            .and_then(|c| softbuffer::Surface::new(c, window.clone()).ok());

        Self {
            surface,
            _context: context,
            window,
            palette,
            adapter: String::new(),
        }
    }

    /// Record the adapter, once there is one.
    pub fn adapter(&mut self, info: &wgpu::AdapterInfo) {
        // The design shows `D3D12 · RTX 4070`. This is the same line carrying
        // real values instead of the mock's — the full adapter name rather than
        // a shortened one, because the point of putting it here is to say
        // exactly which device was picked on a machine that has more than one.
        self.adapter = format!("{} · {}", backend_name(info.backend), info.name);
    }

    /// Paint the splash for `stage`.
    pub fn show(&mut self, stage: Stage) {
        let Some(surface) = self.surface.as_mut() else {
            return;
        };
        let size = self.window.inner_size();
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return;
        };
        if surface.resize(width, height).is_err() {
            return;
        }
        let Ok(mut buffer) = surface.buffer_mut() else {
            return;
        };

        let pixels = render(
            size.width as usize,
            size.height as usize,
            self.window.scale_factor() as f32,
            &self.palette,
            stage,
            &self.adapter,
        );
        buffer.copy_from_slice(&pixels);
        // A failed present is not worth reporting past debug: the window is
        // about to be taken over by wgpu regardless.
        let _ = buffer.present();
    }
}

/// The name people call the backend, rather than the name wgpu's enum uses.
///
/// `{:?}` on `Backend::Dx12` gives "Dx12"; the design writes "D3D12", which is
/// what the API is actually called and what a bug report would say.
fn backend_name(backend: wgpu::Backend) -> &'static str {
    match backend {
        wgpu::Backend::Vulkan => "Vulkan",
        wgpu::Backend::Dx12 => "D3D12",
        wgpu::Backend::Metal => "Metal",
        wgpu::Backend::Gl => "OpenGL",
        wgpu::Backend::BrowserWebGpu => "WebGPU",
        wgpu::Backend::Noop => "no GPU",
    }
}

/// Draw the whole splash into a fresh `0RGB` buffer.
///
/// Pure, so the layout can be rasterised and looked at in a test rather than
/// only ever existing for a few hundred milliseconds at startup.
pub fn render(
    width: usize,
    height: usize,
    scale: f32,
    palette: &Palette,
    stage: Stage,
    adapter: &str,
) -> Vec<u32> {
    let mut buf = Buffer::new(width, height);
    let w = width as f32;
    let h = height as f32;
    // The design states `#101113`, which falls between this palette's `backdrop`
    // (#0D0E10) and `window` (#111214) — three units from one and one from the
    // other. Taking the token rather than the literal is what keeps the splash
    // working in Paper, where a hard-coded near-black would be a jarring flash
    // of the wrong theme in front of a light interface.
    buf.fill(palette.backdrop);

    brand(&mut buf, w, h, scale, palette, Some(TAGLINE));

    // --- the progress block, pinned near the bottom ---
    let status_px = design::STATUS * scale;
    let status = Font::new(status_px, 400.0, 0.0);
    let block_left = w * design::BLOCK_INSET;
    let block_right = w * (1.0 - design::BLOCK_INSET);
    let block_bottom = h * (1.0 - design::BLOCK_BOTTOM);

    let status_baseline = block_bottom;
    let bar_h = design::BAR_HEIGHT * scale;
    let bar_radius = design::BAR_RADIUS * scale;
    let bar_bottom = status_baseline - status_px - design::BAR_GAP * scale;

    // The track first, then the fill over it, so the fill's rounded cap sits on
    // the track rather than knocking a notch out of it.
    buf.rounded_rect(
        block_left,
        bar_bottom - bar_h,
        block_right,
        bar_bottom,
        bar_radius,
        // `rail` is the slider-track token, which is what this is.
        palette.rail,
    );
    let filled = (block_right - block_left) * stage.progress().clamp(0.0, 1.0);
    if filled > 0.0 {
        buf.rounded_rect(
            block_left,
            bar_bottom - bar_h,
            block_left + filled,
            bar_bottom,
            bar_radius,
            palette.accent,
        );
    }

    if let Some(font) = &status {
        // Derived from the backdrop this whole picture is filled with, at the
        // rank `text_dim` held. The splash is the one screen where the pit is
        // the *only* surface, so an ink that cannot be read on it cannot be
        // read at all — and `text_dim` is a mid-grey, which on Krita's mid-grey
        // pit is 1.34:1. See `theme::contrast`.
        let ink = contrast::ink_on(palette.backdrop, Ink::Dim);
        font.draw(
            stage.status(),
            block_left,
            status_baseline,
            |px, py, cov| {
                buf.blend(px, py, ink, cov);
            },
        );
        if !adapter.is_empty() {
            let x = block_right - font.width(adapter);
            font.draw(adapter, x, status_baseline, |px, py, cov| {
                buf.blend(px, py, ink, cov);
            });
        }
    }

    buf.pixels
}

/// The brand group alone, on the theme's backdrop: the splash with its start-up
/// furniture taken off.
///
/// This is what `docshot` puts at the head of the README, and the two omissions
/// are the whole reason it is not just [`render`] at a wide aspect. The progress
/// bar reports work that is not happening in a still picture, and the tagline
/// carries `CARGO_PKG_VERSION` — a *committed* image saying `v0.0.1` is wrong
/// from the next release onwards, and nothing would catch it. What is left is
/// the mark and the wordmark at exactly the geometry [`design`] gives them.
pub fn banner(width: usize, height: usize, scale: f32, palette: &Palette) -> Vec<u32> {
    let mut buf = Buffer::new(width, height);
    buf.fill(palette.backdrop);
    brand(&mut buf, width as f32, height as f32, scale, palette, None);
    buf.pixels
}

/// Width and height of the mark-and-wordmark row at `scale`, in pixels.
///
/// A caller that has to *choose* the field the group sits in — the banner, which
/// has no window to fill — needs the group's size before it can pick one. Kept
/// beside the drawing rather than duplicated at the call site, so a banner
/// cannot come to disagree with the splash about how wide "UMBER" is.
pub fn row_extent(scale: f32) -> (f32, f32) {
    let mark = design::MARK * scale;
    let wordmark_px = design::WORDMARK * scale;
    let font = Font::new(
        wordmark_px,
        design::WORDMARK_WEIGHT,
        design::WORDMARK_TRACKING * scale,
    );
    let wordmark_w = font.as_ref().map_or(0.0, |f| f.width("UMBER"));
    (
        mark + design::MARK_GAP * scale + wordmark_w,
        wordmark_px.max(mark),
    )
}

/// The mark, the wordmark and — when one is given — the tagline, centred in a
/// `w × h` field.
///
/// Split out of [`render`] so [`banner`] draws the same group from the same
/// numbers. The alternative was a second copy of the layout, which would be one
/// more thing to move whenever the design moved the mark or the gap, and no test
/// would notice the day it stopped matching.
fn brand(buf: &mut Buffer, w: f32, h: f32, scale: f32, palette: &Palette, tagline: Option<&str>) {
    let mark = design::MARK * scale;
    let wordmark_px = design::WORDMARK * scale;
    let tagline_px = design::TAGLINE * scale;

    let wordmark = Font::new(
        wordmark_px,
        design::WORDMARK_WEIGHT,
        design::WORDMARK_TRACKING * scale,
    );
    let wordmark_w = wordmark.as_ref().map_or(0.0, |f| f.width("UMBER"));
    let row_w = mark + design::MARK_GAP * scale + wordmark_w;
    // The design gives the wordmark `line-height:1`, so the row is as tall as
    // the type size, and the mark is centred against that rather than the other
    // way round.
    let row_h = wordmark_px.max(mark);
    // With no tagline the group *is* the row. Reserving the space anyway would
    // push the row off centre by half a tagline for the sake of nothing.
    let group_h = match tagline {
        Some(_) => row_h + design::TAGLINE_GAP * scale + tagline_px * 1.2,
        None => row_h,
    };

    let group_top = (h - group_h) * 0.5;
    let row_left = (w - row_w) * 0.5;

    let mark_top = group_top + (row_h - mark) * 0.5;
    buf.rounded_rect(
        row_left,
        mark_top,
        row_left + mark,
        mark_top + mark,
        // The splash's own corner ratio: the design draws this instance at
        // 52 px with an 8 px radius, which is not the ratio the 15 px menu-bar
        // mark uses. See `logo::SPLASH_CORNER_RATIO`.
        mark * logo::SPLASH_CORNER_RATIO,
        palette.accent,
    );

    if let Some(font) = &wordmark {
        // Cap height rather than the em box: the design aligns the wordmark
        // optically with the square beside it, and capitals are what the eye
        // lines up.
        let baseline = group_top + (row_h + font.cap_height()) * 0.5;
        let x = row_left + mark + design::MARK_GAP * scale;
        font.draw("UMBER", x, baseline, |px, py, coverage| {
            buf.blend(px, py, palette.text_strong, coverage);
        });
    }

    let Some(line) = tagline else { return };
    if let Some(font) = Font::new(tagline_px, 400.0, design::TAGLINE_TRACKING * scale) {
        let baseline = group_top + row_h + design::TAGLINE_GAP * scale + font.cap_height();
        let x = (w - font.width(line)) * 0.5;
        // The design's #6e7176 is dimmer than any token here, and `text_dim`
        // was the nearest. It is derived from the backdrop now, for the reason
        // the status line beside it is: this picture is *nothing but* the
        // backdrop, so a supporting line has to be readable on whatever that
        // is. On Graphite the two answers are within a couple of levels.
        let ink = contrast::ink_on(palette.backdrop, Ink::Dim);
        font.draw(line, x, baseline, |px, py, coverage| {
            buf.blend(px, py, ink, coverage);
        });
    }
}

/// A plain CPU framebuffer in softbuffer's `0RGB` layout.
struct Buffer {
    width: usize,
    height: usize,
    pixels: Vec<u32>,
}

impl Buffer {
    fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width * height],
        }
    }

    fn fill(&mut self, colour: Color32) {
        self.pixels.fill(pack(colour));
    }

    /// Blend `colour` over the pixel at `(x, y)` with the given coverage.
    ///
    /// Straight arithmetic in sRGB rather than the linear blending the engine
    /// uses everywhere else, and on purpose: this is antialiasing coverage over
    /// flat UI colours, which is exactly the case where compositing in display
    /// space is what looks right — the same thing egui does for the rest of the
    /// interface.
    fn blend(&mut self, x: i32, y: i32, colour: Color32, coverage: f32) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let coverage = coverage.clamp(0.0, 1.0);
        let i = y as usize * self.width + x as usize;
        let dst = self.pixels[i];
        let mix = |shift: u32, src: u8| {
            let d = ((dst >> shift) & 0xFF) as f32;
            let v = d + (src as f32 - d) * coverage;
            (v.round().clamp(0.0, 255.0) as u32) << shift
        };
        self.pixels[i] = mix(16, colour.r()) | mix(8, colour.g()) | mix(0, colour.b());
    }

    /// Fill a rounded rectangle, antialiased from the same distance field the
    /// application icon is rasterised with.
    fn rounded_rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, radius: f32, colour: Color32) {
        let half_w = (x1 - x0) * 0.5;
        let half_h = (y1 - y0) * 0.5;
        if half_w <= 0.0 || half_h <= 0.0 {
            return;
        }
        let (cx, cy) = (x0 + half_w, y0 + half_h);

        // One pixel of slack so the antialiased edge is not clipped.
        let lo_x = (x0.floor() as i32 - 1).max(0);
        let hi_x = (x1.ceil() as i32 + 1).min(self.width as i32);
        let lo_y = (y0.floor() as i32 - 1).max(0);
        let hi_y = (y1.ceil() as i32 + 1).min(self.height as i32);

        for y in lo_y..hi_y {
            for x in lo_x..hi_x {
                let coverage = logo::rounded_box_coverage(
                    x as f32 + 0.5 - cx,
                    y as f32 + 0.5 - cy,
                    half_w,
                    half_h,
                    radius,
                );
                if coverage > 0.0 {
                    self.blend(x, y, colour, coverage);
                }
            }
        }
    }
}

fn pack(colour: Color32) -> u32 {
    ((colour.r() as u32) << 16) | ((colour.g() as u32) << 8) | colour.b() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ThemeKind;

    const W: usize = 1440;
    const H: usize = 900;

    fn frame(stage: Stage, adapter: &str) -> Vec<u32> {
        render(W, H, 1.0, &Palette::of(ThemeKind::Graphite), stage, adapter)
    }

    #[test]
    fn the_splash_fills_the_window() {
        assert_eq!(frame(Stage::Adapter, "").len(), W * H);
    }

    #[test]
    fn the_background_is_the_palettes_backdrop() {
        let px = frame(Stage::Adapter, "");
        // A corner, far from anything the splash draws.
        assert_eq!(px[0], pack(Palette::of(ThemeKind::Graphite).backdrop));
    }

    #[test]
    fn the_mark_is_drawn_in_the_accent() {
        let palette = Palette::of(ThemeKind::Graphite);
        let px = frame(Stage::Adapter, "");
        assert!(
            px.iter().any(|&p| p == pack(palette.accent)),
            "no fully accent-coloured pixel — the mark and the bar are both missing"
        );
    }

    #[test]
    fn the_wordmark_is_actually_drawn() {
        // `text_strong` appears nowhere else on the splash, so its presence is
        // proof that Archivo parsed, instanced and rasterised. Without this a
        // font failure would degrade to a silently wordless splash.
        let palette = Palette::of(ThemeKind::Graphite);
        let px = frame(Stage::Adapter, "");
        let lit = px
            .iter()
            .filter(|&&p| p == pack(palette.text_strong))
            .count();
        assert!(
            lit > 500,
            "only {lit} wordmark pixels — is Archivo loading?"
        );
    }

    #[test]
    fn the_bar_grows_with_the_stage_and_never_shrinks() {
        let palette = Palette::of(ThemeKind::Graphite);
        let accent = pack(palette.accent);
        // Count accent pixels along the bar's centre line, derived from the same
        // formula the renderer lays it out with. The mark never reaches this
        // row — it sits in the middle of the window, the bar near the base.
        let bar_centre = H as f32 * (1.0 - design::BLOCK_BOTTOM)
            - design::STATUS
            - design::BAR_GAP
            - design::BAR_HEIGHT * 0.5;
        let row = bar_centre as usize;
        let width_at = |stage| {
            frame(stage, "")[row * W..(row + 1) * W]
                .iter()
                .filter(|&&p| p == accent)
                .count()
        };

        let stages = [
            Stage::Adapter,
            Stage::Surface,
            Stage::Shaders,
            Stage::Fonts,
            Stage::Ready,
        ];
        let mut previous = 0;
        for stage in stages {
            let w = width_at(stage);
            assert!(
                w >= previous,
                "the bar went backwards at {stage:?}: {w} < {previous}"
            );
            previous = w;
        }
        assert!(previous > 0, "the bar never filled at all");
    }

    #[test]
    fn every_stage_reports_progress_inside_the_bar() {
        for stage in [
            Stage::Adapter,
            Stage::Surface,
            Stage::Shaders,
            Stage::Fonts,
            Stage::Ready,
        ] {
            let p = stage.progress();
            assert!((0.0..=1.0).contains(&p), "{stage:?} reports {p}");
            assert!(!stage.status().is_empty());
        }
        assert_eq!(Stage::Ready.progress(), 1.0);
    }

    #[test]
    fn the_splash_follows_the_theme() {
        // A hard-coded near-black background would make Paper start with a
        // flash of the wrong theme. This is the guard on that.
        let paper = render(W, H, 1.0, &Palette::of(ThemeKind::Paper), Stage::Ready, "");
        assert_eq!(paper[0], pack(Palette::of(ThemeKind::Paper).backdrop));
        assert_ne!(paper[0], pack(Palette::of(ThemeKind::Graphite).backdrop));
    }

    #[test]
    fn it_survives_a_window_too_small_to_lay_out_in() {
        // Windows can be dragged to a sliver, and a splash that panicked while
        // the GPU was still coming up would take the application with it.
        for (w, h) in [(1, 1), (4, 400), (400, 4), (60, 60)] {
            let px = render(
                w,
                h,
                1.0,
                &Palette::of(ThemeKind::Graphite),
                Stage::Ready,
                "",
            );
            assert_eq!(px.len(), w * h);
        }
    }

    #[test]
    fn a_fractional_scale_factor_still_lays_out() {
        for scale in [1.0, 1.25, 1.5, 2.0, 3.0] {
            let px = render(
                W,
                H,
                scale,
                &Palette::of(ThemeKind::Graphite),
                Stage::Shaders,
                "D3D12 · NVIDIA GeForce RTX 4070",
            );
            assert_eq!(px.len(), W * H);
        }
    }

    /// The banner is the splash's brand group and nothing else. These three pin
    /// each half of that: the group is there, and neither piece of start-up
    /// furniture came with it.
    #[test]
    fn the_banner_carries_the_mark_and_the_wordmark() {
        let palette = Palette::of(ThemeKind::Graphite);
        let px = banner(600, 220, 1.0, &palette);
        assert!(
            px.iter().any(|&p| p == pack(palette.accent)),
            "no accent pixel — the mark is missing"
        );
        let lit = px
            .iter()
            .filter(|&&p| p == pack(palette.text_strong))
            .count();
        assert!(
            lit > 500,
            "only {lit} wordmark pixels — is Archivo loading?"
        );
    }

    #[test]
    fn the_banner_has_no_progress_bar() {
        // `rail` is the bar's track and is used nowhere else on the splash, so
        // its absence is the whole assertion.
        let palette = Palette::of(ThemeKind::Graphite);
        let px = banner(600, 220, 1.0, &palette);
        assert!(!px.iter().any(|&p| p == pack(palette.rail)));
    }

    #[test]
    fn the_banner_omits_the_version() {
        // The tagline and the status line are the only supporting ink on the
        // splash, and the tagline is where `CARGO_PKG_VERSION` appears. A
        // committed picture carrying a version number is wrong from the next
        // release on.
        //
        // The ink is `contrast::ink_on(backdrop, Dim)` rather than `text_dim`
        // now, and this asks it the same way `render` computes it — one
        // statement of what colour the supporting lines are, so the test cannot
        // go on passing by counting a colour nothing draws.
        let palette = Palette::of(ThemeKind::Graphite);
        let px = banner(600, 220, 1.0, &palette);
        let supporting = pack(contrast::ink_on(palette.backdrop, Ink::Dim));
        assert_eq!(px.iter().filter(|&&p| p == supporting).count(), 0);
    }

    /// The supporting lines are drawn in the ink derived from the pit, and
    /// `text_dim` has left the splash entirely.
    ///
    /// Two halves, because either alone passes for the wrong reason. The
    /// positive one counts pixels of the exact derived colour — at scale 3 the
    /// tagline is 34 points of Archivo and has plenty of fully covered texels,
    /// where at scale 1 it has one, which is why this does not reuse the
    /// splash's own test size. The negative one *mutates* `text_dim` to
    /// magenta and requires the picture not to move: a positive test cannot
    /// tell an ink that was changed from one that was copied, and this is the
    /// only reading that says the old token is gone rather than merely joined.
    #[test]
    fn the_supporting_lines_are_derived_from_the_pit() {
        for kind in [ThemeKind::Graphite, ThemeKind::Paper, ThemeKind::Krita] {
            let palette = Palette::of(kind);
            let px = render(900, 500, 3.0, &palette, Stage::Ready, "");
            let supporting = pack(contrast::ink_on(palette.backdrop, Ink::Dim));
            let lit = px.iter().filter(|&&p| p == supporting).count();
            assert!(lit > 100, "{kind:?}: only {lit} pixels of supporting ink");
            // And it is not the token it replaced, or the theme this was broken
            // for would be passing on the old colour.
            assert_ne!(supporting, pack(palette.text_dim), "{kind:?}");

            let mut moved = palette;
            moved.text_dim = Color32::from_rgb(0xFF, 0x00, 0xFF);
            assert_eq!(
                px,
                render(900, 500, 3.0, &moved, Stage::Ready, ""),
                "{kind:?}: something on the splash still draws text_dim",
            );
        }
    }

    /// The row the banner sizes its field from has to be the row the banner
    /// actually draws, or the margins come out lopsided.
    #[test]
    fn the_reported_row_is_the_row_that_is_drawn() {
        let palette = Palette::of(ThemeKind::Graphite);
        let scale = 2.0;
        let (row_w, row_h) = row_extent(scale);
        let (w, h) = ((row_w + 80.0) as usize, (row_h + 80.0) as usize);
        let px = banner(w, h, scale, &palette);

        // Columns holding anything other than the backdrop, which for a banner
        // is exactly the group.
        let backdrop = pack(palette.backdrop);
        let mut left = w;
        let mut right = 0;
        for y in 0..h {
            for x in 0..w {
                if px[y * w + x] != backdrop {
                    left = left.min(x);
                    right = right.max(x);
                }
            }
        }
        let drawn = (right + 1 - left) as f32;
        // Within a pixel or two: the mark's antialiased edge and the wordmark's
        // side bearings both round.
        assert!(
            (drawn - row_w).abs() <= 3.0,
            "reported {row_w}, drew {drawn}"
        );
    }

    /// Write the splash out as PNGs so its layout can actually be looked at.
    ///
    /// Ignored: it exists to be run by hand when the design changes.
    ///
    /// ```sh
    /// cargo test -p umber-app splash_preview -- --ignored
    /// ```
    #[test]
    #[ignore = "writes preview PNGs; run deliberately when changing the layout"]
    fn splash_preview() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/splash");
        std::fs::create_dir_all(&dir).expect("create preview directory");

        for (name, stage, adapter) in [
            ("1-adapter", Stage::Adapter, ""),
            (
                "2-surface",
                Stage::Surface,
                "D3D12 · NVIDIA GeForce RTX 4070",
            ),
            (
                "3-shaders",
                Stage::Shaders,
                "D3D12 · NVIDIA GeForce RTX 4070",
            ),
            ("4-ready", Stage::Ready, "D3D12 · NVIDIA GeForce RTX 4070"),
        ] {
            let px = render(W, H, 1.0, &Palette::of(ThemeKind::Graphite), stage, adapter);
            let mut rgba = Vec::with_capacity(W * H * 4);
            for p in px {
                rgba.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8, 255]);
            }
            let file = std::fs::File::create(dir.join(format!("{name}.png"))).expect("create png");
            let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), W as u32, H as u32);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .expect("png header")
                .write_image_data(&rgba)
                .expect("png data");
        }
    }
}
