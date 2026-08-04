//! The pictures in `docs/images/`, drawn by the interface that they are
//! pictures of.
//!
//! ```sh
//! cargo run -p umber-app --example docs-images
//! ```
//!
//! The images are committed, like `assets/icons/` and the generated
//! `tip_table.rs`, and this module is how they are made again. It is run by
//! hand: nothing in `cargo test` touches it, because it wants a GPU adapter and
//! writes into the working tree.
//!
//! **Nothing here draws an interface.** The banner is `splash::banner`, the
//! dialog is `settings::show` and the pickers are `panels::panel` — the same
//! functions the application calls, handed an offscreen target instead of a
//! window. That is the same rule the brush list's samples follow (stamped from
//! the brush, not drawn from two of its numbers) and the icon PNGs (rasterised
//! from `logo.rs`, not exported from a drawing program), and for the same
//! reason: a picture of the interface that something else drew goes stale in
//! silence, and a README is exactly where nobody looks for the drift.
//!
//! **`docs/images/window.png` is the exception, and is not written here.** It
//! is a photograph of a real session — a document with actual work on it, which
//! is the one thing this module cannot produce, since `ui::draw` leaves the
//! canvas region empty for the GPU composite that never runs offscreen. It is
//! therefore the one picture that will not follow the interface, and it has to
//! be retaken by hand when the workspace changes shape. Regenerating everything
//! else leaves it alone.
//!
//! Three things about the offscreen render are worth knowing before changing it:
//!
//! - **The target is `Rgba8Unorm`, deliberately non-sRGB**, matching the real
//!   surface. egui emits colours that are already gamma-encoded and picks its
//!   fragment entry point off the format: a non-sRGB target gets
//!   `fs_main_gamma_framebuffer`, which writes those bytes through. An sRGB
//!   target would encode them a second time and every picture would come out
//!   washed out — the same trap `composite.wgsl` is written around. [`Stage::new`]
//!   proves it rather than assuming it, by painting one flat token and reading
//!   the byte back.
//! - **The supersampling goes through the viewport's native scale, not egui's
//!   zoom factor.** They both end up in `pixels_per_point`, but the zoom factor
//!   is what the settings dialog's *Interface scale* slider reads and writes —
//!   drawing at twice the size through it would put "200%" in a picture of the
//!   default settings.
//! - **Frames are run until egui stops asking for another.** It measures a
//!   layout against the previous frame's, so the first pass lays a modal out
//!   against a screen it has not seen — and, worse, it animates. A fixed three
//!   frames caught the settings dialog halfway through its fade-in, at about
//!   half opacity, which looks like a muted design rather than a bug. `settled`
//!   in [`Stage::shoot`] is the guard on that.

use crate::dock::{Layout, PanelKind};
use crate::editor::Editor;
use crate::settings::SettingsTab;
use crate::theme::{self, Palette, ThemeKind};
use crate::{panels, prefs, settings, shortcuts, splash, ui};
use egui::{Color32, Pos2, Rect, Vec2, vec2};
use std::path::{Path, PathBuf};
use umber_core::Color;
use umber_render::Gpu;

/// Must match the real surface: non-sRGB, so egui's gamma-space output is
/// written through rather than encoded twice. See the module comment.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Device pixels per egui point for a docked panel.
///
/// A panel is only 264 points wide, so one pixel per point is a picture too
/// small to read; at two it is 528, which is about the width a README shows a
/// column image at, and the file is still under a hundred kilobytes.
const PANEL_SCALE: f32 = 2.0;

/// The settings dialog is already 1048 points across, which is wider than a
/// README ever displays an image. One and a half is enough to survive being
/// scaled down and keeps a picture that is mostly text under a quarter of a
/// megabyte; two doubled the file for resolution nothing would show.
const SETTINGS_SCALE: f32 = 1.5;

/// The banner is drawn at three device pixels per design point: it is pure
/// geometry and type with no interface behind it, so the extra resolution costs
/// a couple of kilobytes and buys a mark whose corner is smooth at any size.
const BANNER_SCALE: f32 = 3.0;

/// Frames run before the picture is worth keeping, and the ceiling on waiting
/// for one that settles.
///
/// egui asks for another frame while anything is still animating. Running until
/// it stops asking is what makes the picture the *settled* interface: the
/// settings modal fades in over egui's default 83 ms, and a fixed three frames
/// caught the entire dialog at roughly half opacity — which read as a design
/// choice rather than as a bug, which is exactly how this sort of thing gets
/// committed. The ceiling is there because a request for an immediate repaint
/// every frame is a thing egui can legitimately do.
const MIN_FRAMES: usize = 2;
const MAX_FRAMES: usize = 90;

/// The settings dialog is 1000×640 and clamps itself to `available - 48`, so a
/// field 48 points wider and taller than the design's size is exactly the dialog
/// at full size with an even 24-point margin of dimmed backdrop around it. Any
/// larger and the picture is mostly empty; any smaller and the dialog shrinks.
const SETTINGS_FIELD: Vec2 = vec2(1048.0, 688.0);

/// Height the Colour module is drawn into before the picture is trimmed back to
/// what it painted.
///
/// Deliberately more than any picker needs. A panel taller than its content is
/// what the application shows when the splitter is dragged down, so nothing here
/// depends on the number being right — [`Image::trim`] takes the slack off, and
/// each mode ends up as tall as it happens to be rather than needing a constant
/// per mode that would go stale the first time a control moved.
const PICKER_FIELD: Vec2 = vec2(theme::metrics::PANEL, 400.0);

/// Space left under the last thing a panel draws, in points — the panel's own
/// horizontal padding, so the trimmed edge matches the two beside it.
const TRIM_MARGIN: f32 = theme::metrics::PANEL_PAD as f32;

/// What the settings footer says instead of this machine's preferences path.
///
/// The three shapes are the ones `prefs`'s module comment documents; the
/// Windows one is written because the pictures are made on Windows and a
/// screenshot cannot show three. It is the path in the form the documentation
/// states it, which is what a reader needs — the expanded form would be one
/// contributor's home directory. See `prefs::set_config_path_label`.
const GENERIC_CONFIG_PATH: &str = r"%APPDATA%\Umber\config\preferences.conf";

/// One finished picture, in the order a PNG wants its bytes.
pub(crate) struct Image {
    width: u32,
    height: u32,
    /// Three bytes per pixel. The alpha is dropped on readback: every one of
    /// these is opaque, and carrying the channel is a quarter of the file for
    /// nothing.
    rgb: Vec<u8>,
}

impl Image {
    fn pixel(&self, x: u32, y: u32) -> Color32 {
        let i = ((y * self.width + x) * 3) as usize;
        Color32::from_rgb(self.rgb[i], self.rgb[i + 1], self.rgb[i + 2])
    }

    /// Cut the picture back to the last row that is not pure `background`,
    /// leaving `margin` device pixels under it.
    ///
    /// A rule rather than a measurement, and that is the point: a panel drawn
    /// into a field taller than its content comes out as tall as its content,
    /// whatever the content turns out to be. The alternative was a height per
    /// picker mode, three constants that would each be silently wrong the first
    /// time a control was added to one.
    fn trim(mut self, background: Color32, margin: u32) -> Self {
        let last = (0..self.height)
            .rev()
            .find(|&y| (0..self.width).any(|x| self.pixel(x, y) != background));
        let Some(last) = last else { return self };
        let height = (last + 1 + margin).min(self.height);
        self.rgb.truncate((height * self.width * 3) as usize);
        self.height = height;
        self
    }
}

/// Draw every picture into `<root>/docs/images`, reporting what was written.
///
/// The one entry point, so `examples/docs-images.rs` needs nothing else public.
/// Errors are strings because every one of them ends up in front of a person
/// running a generator by hand, and none of them is worth matching on.
pub fn generate(root: &Path) -> Result<(), String> {
    let dir = root.join("docs").join("images");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;

    // `settings::show` reads the preferences file on its first call and applies
    // it to whatever editor and context it is handed, and publishes the stored
    // shortcut table globally. These pictures show Umber as it ships, not as
    // this machine happens to be set up, so the once-only latch is spent here on
    // a context and an editor that are thrown away, and the binding table is put
    // back to the factory one.
    let mut scratch = Editor::default();
    prefs::ensure_loaded(&egui::Context::default(), &mut scratch);
    shortcuts::publish(shortcuts::defaults());
    // And the footer of every settings pane names the file, which means it names
    // the account this is running as. See `prefs::set_config_path_label`.
    prefs::set_config_path_label(GENERIC_CONFIG_PATH);

    let mut written = Vec::new();

    // The banner needs no GPU — it is `splash::banner`, which paints on the CPU
    // precisely so it can run before there is a device.
    for (kind, name) in [
        (ThemeKind::Graphite, "banner.png"),
        // The banner sits on the README's own background rather than inside the
        // application, so unlike the interface shots it is worth having in both
        // themes: a `<picture>` element can then follow the reader's.
        (ThemeKind::Paper, "banner-paper.png"),
    ] {
        written.push(banner(&dir, kind, name)?);
    }

    match Stage::new() {
        Some(mut stage) => {
            written.extend(interface(&mut stage, &dir)?);
        }
        None => {
            // Skipping is the honest answer. A committed blank PNG would be
            // worse than an absent one, which is also why the GPU tests skip.
            eprintln!(
                "no GPU adapter: the interface pictures need one and were skipped. \
                 The banner was written."
            );
        }
    }

    for (path, image_bytes, w, h) in &written {
        println!("{:>7} bytes  {w}x{h}  {}", image_bytes, path.display());
    }
    Ok(())
}

/// The mark and the wordmark, on the theme's backdrop.
fn banner(dir: &Path, kind: ThemeKind, name: &str) -> Result<(PathBuf, u64, u32, u32), String> {
    let (row_w, row_h) = splash::row_extent(BANNER_SCALE);
    // The field is set from the group's own height rather than chosen, so it
    // stays in proportion if the design ever changes the mark size or the
    // wordmark's. Wider than it is tall around the group, because a banner is
    // read as a strip and a square of margin makes it look like a logotype that
    // failed to fill its box.
    let width = (row_w + row_h * 2.0).round().max(1.0) as usize;
    let height = (row_h * 2.4).round().max(1.0) as usize;

    let packed = splash::banner(width, height, BANNER_SCALE, &Palette::of(kind));
    // softbuffer's layout is `0RGB` in a u32; a PNG wants the three bytes.
    let mut rgb = Vec::with_capacity(width * height * 3);
    for p in packed {
        rgb.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
    }
    let image = Image {
        width: width as u32,
        height: height as u32,
        rgb,
    };
    write_png(&dir.join(name), &image)
}

/// Everything that needs egui and a device.
fn interface(stage: &mut Stage, dir: &Path) -> Result<Vec<(PathBuf, u64, u32, u32)>, String> {
    let mut out = Vec::new();

    // Themes first: it is the pane the design draws in full, and the one that
    // shows most of what the dialog is — the two themes as live cards, the four
    // accents, and the layout controls under them. Shortcuts is the other,
    // and is the only place the binding table can be seen at all.
    for (tab, name) in [
        (SettingsTab::Themes, "settings-themes.png"),
        (SettingsTab::Shortcuts, "settings-shortcuts.png"),
    ] {
        out.push(settings_shot(stage, dir, tab, name)?);
    }

    for (picker, shape, name) in [
        (
            crate::colorpicker::PickerMode::Wheel,
            crate::colorpicker::WheelShape::Triangle,
            "picker-wheel.png",
        ),
        (
            crate::colorpicker::PickerMode::Wheel,
            crate::colorpicker::WheelShape::Square,
            "picker-wheel-square.png",
        ),
        (
            crate::colorpicker::PickerMode::Square,
            crate::colorpicker::WheelShape::Triangle,
            "picker-square.png",
        ),
        (
            crate::colorpicker::PickerMode::Sliders,
            crate::colorpicker::WheelShape::Triangle,
            "picker-sliders.png",
        ),
    ] {
        out.push(picker_shot(stage, dir, picker, shape, name)?);
    }

    // The Brushes module, which is the one panel that shows real work being
    // done without a document: every row's sample is a stroke stamped by the
    // dab generator on the CPU, so a picture of the list is a picture of two
    // hundred brushes actually painting.
    out.push(panel_shot(
        stage,
        dir,
        PanelKind::Brushes,
        vec2(theme::metrics::PANEL, 460.0),
        |_| {},
        "brushes.png",
    )?);

    // The Layers module, holding a folder, a link group and a couple of ticks.
    //
    // The thumbnails come out as bare checker, and that is honest rather than a
    // shortcoming of the shot: they are read back off a canvas, this document
    // has none, and "nothing on this layer" is exactly the checker a real empty
    // layer draws. A picture of the *structure* is what this is for.
    out.push(panel_shot(
        stage,
        dir,
        PanelKind::Layers,
        vec2(theme::metrics::PANEL, 320.0),
        |ed| {
            for _ in 0..3 {
                ed.layers.add();
            }
            for (n, index) in (0..4).enumerate() {
                if let Some(layer) = ed.layers.get_mut(index) {
                    layer.name = ["Background", "Flats", "Line", "Shading"][n].to_string();
                }
            }
            // A group holding the top two, so the row that folds and the rows
            // stepped in beneath it are both in the picture.
            ed.layers.group(&[2, 3]);
            if let Some(folder) = ed.layers.get_mut(4) {
                folder.name = "Ink".to_string();
            }
            // And a link between the two that are not in it, which is what puts
            // a coloured chain on a row.
            ed.layers.link(&[0, 1]);
            ed.layers.set_active(2);
        },
        "layers.png",
    )?);

    Ok(out)
}

/// One docked module, at the width the design gives a panel.
///
/// `setup` is handed the editor before it is drawn, for the panels whose
/// picture is of a document rather than of the controls themselves.
fn panel_shot(
    stage: &mut Stage,
    dir: &Path,
    kind: PanelKind,
    field: Vec2,
    setup: impl FnOnce(&mut Editor),
    name: &str,
) -> Result<(PathBuf, u64, u32, u32), String> {
    let mut ed = editor();
    setup(&mut ed);
    let palette = ed.palette();
    let rect = Rect::from_min_size(Pos2::ZERO, field);
    let image = stage
        .shoot(field, PANEL_SCALE, &palette, palette.dock, |root| {
            let mut actions = ui::UiActions::default();
            panels::panel(root, &palette, &mut ed, &mut actions, kind, rect);
        })
        .trim(palette.dock, (TRIM_MARGIN * PANEL_SCALE) as u32);
    write_png(&dir.join(name), &image)
}

/// The settings dialog, open on one pane.
fn settings_shot(
    stage: &mut Stage,
    dir: &Path,
    tab: SettingsTab,
    name: &str,
) -> Result<(PathBuf, u64, u32, u32), String> {
    let mut ed = editor();
    ed.ui.settings_open = true;
    ed.ui.settings_tab = tab;
    // An empty theme library, so the Themes pane shows the two Umber ships with
    // and nothing else. Left alone it reads the *user's* directory — and this
    // picture is committed, so a card for every theme the person regenerating
    // it happens to have would publish their workspace in the README. It is the
    // leak `prefs::set_config_path_label` exists to stop, one door over.
    settings::stage_themes(&stage.ctx, crate::themelib::ThemeLibrary::default());
    let palette = ed.palette();

    // The modal dims whatever is behind it, and behind it here is nothing, so
    // the clear is the backdrop the dialog would be dimming in the application.
    let image = stage.shoot(
        SETTINGS_FIELD,
        SETTINGS_SCALE,
        &palette,
        palette.backdrop,
        // The dialog reports what it was asked to do — reveal the autosave
        // folder, for one — and here there is nobody to carry that out. A
        // picture of a dialog is not a session, so the requests are discarded.
        |ui| settings::show(ui, &palette, &mut ed, &mut crate::ui::UiActions::default()),
    );
    write_png(&dir.join(name), &image)
}

/// The Colour module, in one of its picker modes.
fn picker_shot(
    stage: &mut Stage,
    dir: &Path,
    picker: crate::colorpicker::PickerMode,
    shape: crate::colorpicker::WheelShape,
    name: &str,
) -> Result<(PathBuf, u64, u32, u32), String> {
    let mut ed = editor();
    ed.ui.picker = picker;
    ed.ui.wheel_shape = shape;
    // A picker showing the editor's start-up near-black puts every marker in one
    // dark corner and the triangle reads as a grey wedge. Posing the shot on a
    // colour the picker can actually show is the same licence `brush_sample`
    // takes when it seeds a pressure ramp — and the colour is the palette's own
    // accent rather than a number typed here, so it follows the theme.
    let accent = palette_colour(ed.palette().accent);
    ed.set_color(accent);
    let palette = ed.palette();

    let rect = Rect::from_min_size(Pos2::ZERO, PICKER_FIELD);
    // `p.dock` is the sidebar column's fill, which `panels::sidebars` supplies
    // as its frame; the panel itself paints no background of its own — which is
    // also what lets the trim below find where the panel's content stops.
    let image = stage
        .shoot(PICKER_FIELD, PANEL_SCALE, &palette, palette.dock, |root| {
            let mut actions = ui::UiActions::default();
            panels::panel(
                root,
                &palette,
                &mut ed,
                &mut actions,
                PanelKind::Colour,
                rect,
            );
        })
        .trim(palette.dock, (TRIM_MARGIN * PANEL_SCALE) as u32);
    write_png(&dir.join(name), &image)
}

/// An editor that owes nothing to the machine this runs on.
///
/// `Editor::default` reads the saved dock arrangement, which is a real file
/// belonging to whoever is running this. A panel drawn from somebody's own
/// layout would be a picture of their workspace rather than of Umber's.
fn editor() -> Editor {
    let mut ed = Editor::default();
    ed.layout = Layout::default();
    ed
}

fn palette_colour(c: Color32) -> Color {
    Color::from_srgb_u8(c.r(), c.g(), c.b(), 255)
}

// ---------------------------------------------------------------------------
// The offscreen egui renderer
// ---------------------------------------------------------------------------

/// A device, a context and an egui renderer, reused across every shot.
///
/// One of each for the whole run, for the reason `gpu_pipeline.rs` shares its:
/// device creation is by far the slowest thing here, and a context per picture
/// would rebuild the Archivo atlas each time.
/// A GPU, an egui context and a renderer, drawing into a texture.
///
/// `pub(crate)` rather than private because it is the only way anything in this
/// crate can *look at* a piece of interface: it draws through `ui`'s own
/// functions into an offscreen target, which is what a screen-by-screen preview
/// needs and what no `cargo test` assertion can stand in for. See
/// `updatedlg`'s ignored `update_dialog_preview`, which is the same idiom
/// `splash`'s `splash_preview` already uses on the CPU. Nothing outside the
/// crate sees it — `generate` is still the whole of this module's public
/// surface.
pub(crate) struct Stage {
    gpu: Gpu,
    /// `pub(crate)` for the reason `Stage` itself is: a caller that wants to
    /// seed the context before the interface reads it — `settings::stage_themes`
    /// is the one — has to reach the context this will draw with, and there is
    /// only one of it for the whole run.
    pub(crate) ctx: egui::Context,
    renderer: egui_wgpu::Renderer,
}

impl Stage {
    /// `None` when there is no adapter — a headless runner, a machine with no
    /// working driver. The caller skips rather than writing a broken file.
    pub(crate) fn new() -> Option<Self> {
        let gpu = pollster::block_on(Gpu::new(Gpu::create_instance(), None))
            .inspect_err(|e| eprintln!("{e}"))
            .ok()?;
        let ctx = egui::Context::default();
        theme::install_fonts(&ctx);
        let renderer =
            egui_wgpu::Renderer::new(&gpu.device, FORMAT, egui_wgpu::RendererOptions::default());

        let mut stage = Self { gpu, ctx, renderer };
        stage.check_colour_space();
        Some(stage)
    }

    /// Paint one flat palette token through the whole pipeline and read the byte
    /// back.
    ///
    /// The colour space here has exactly one way to go wrong and it is silent:
    /// an sRGB target would send egui down `fs_main_linear_framebuffer` and
    /// every picture would come out pale, which looks like a design choice
    /// rather than a bug. This is the cheap proof that the bytes coming out are
    /// the bytes the theme asked for.
    fn check_colour_space(&mut self) {
        let palette = Palette::of(ThemeKind::Graphite);
        let image = self.shoot(vec2(32.0, 32.0), 1.0, &palette, Color32::BLACK, |ui| {
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(palette.chrome))
                .show(ui, |_| {});
        });
        let got = image.pixel(image.width / 2, image.height / 2);
        if got != palette.chrome {
            eprintln!(
                "the offscreen target is not writing palette colours through: \
                 asked for {:?}, got {got:?}. Is {FORMAT:?} still non-sRGB?",
                palette.chrome
            );
        }
    }

    /// Run `body` until the interface stops animating, then read the frame back.
    ///
    /// `size` is in egui points; the image comes out `scale` times that.
    pub(crate) fn shoot(
        &mut self,
        size: Vec2,
        scale: f32,
        palette: &Palette,
        background: Color32,
        mut body: impl FnMut(&mut egui::Ui),
    ) -> Image {
        theme::apply(&self.ctx, palette);

        let mut last = None;
        for frame in 0..MAX_FRAMES {
            let mut input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                // A clock rather than `None`, which egui reads as "one frame at
                // the predicted rate". Animations then advance by the same
                // amount per frame on every machine.
                time: Some(frame as f64 / 60.0),
                ..Default::default()
            };
            // The viewport's native scale, not `Context::set_zoom_factor`. Both
            // reach `pixels_per_point`, but the zoom factor is the *Interface
            // scale* setting, and drawing at twice the size through it would put
            // "200%" in a picture of the defaults. See the module comment.
            input
                .viewports
                .entry(input.viewport_id)
                .or_default()
                .native_pixels_per_point = Some(scale);

            let output = self.ctx.run_ui(input, |ui| body(ui));
            // Every frame's deltas, not only the kept one's: egui sends the
            // whole font atlas once and only new regions afterwards, so dropping
            // an early delta leaves the renderer patching a texture it never
            // allocated.
            for (id, delta) in &output.textures_delta.set {
                self.renderer
                    .update_texture(&self.gpu.device, &self.gpu.queue, *id, delta);
            }
            // `Duration::MAX` is egui's "nothing pending" — the same reading
            // `app.rs` schedules repaints from. Anything shorter means something
            // is still moving, and a picture taken now would be a frame partway
            // through it.
            let settled = output
                .viewport_output
                .get(&self.ctx.viewport_id())
                .is_some_and(|out| out.repaint_delay == std::time::Duration::MAX);
            last = Some(output);
            if frame + 1 >= MIN_FRAMES && settled {
                break;
            }
        }

        let output = last.expect("MAX_FRAMES is not zero");
        let ppp = output.pixels_per_point;
        let jobs = self.ctx.tessellate(output.shapes, ppp);
        let width = (size.x * ppp).round().max(1.0) as u32;
        let height = (size.y * ppp).round().max(1.0) as u32;

        let target = self.gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("docshot"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("docshot"),
            });
        let descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [width, height],
            pixels_per_point: ppp,
        };
        self.renderer.update_buffers(
            &self.gpu.device,
            &self.gpu.queue,
            &mut encoder,
            &jobs,
            &descriptor,
        );
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("docshot"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        // The format is not sRGB, so a clear value goes into the
                        // texture as written and this is exactly `background`.
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(background.r()) / 255.0,
                            g: f64::from(background.g()) / 255.0,
                            b: f64::from(background.b()) / 255.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.renderer
                .render(&mut pass.forget_lifetime(), &jobs, &descriptor);
        }

        let image = self.read_back(&mut encoder, &target, width, height);
        for id in &output.textures_delta.free {
            self.renderer.free_texture(id);
        }
        image
    }

    /// Copy the target into a staging buffer, wait for it, and drop the alpha.
    fn read_back(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Image {
        // A texture-to-buffer copy needs rows aligned to 256 bytes, which four
        // bytes per pixel only reaches by accident.
        let unpadded = width * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;

        let staging = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("docshot-readback"),
            size: u64::from(padded) * u64::from(height),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        // The encoder was handed in half-built, so this submits the render pass
        // and the copy together.
        let finished = std::mem::replace(
            encoder,
            self.gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None }),
        );
        self.gpu.queue.submit(Some(finished.finish()));

        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.gpu.device.poll(wgpu::PollType::wait_indefinitely());
        let mapped = slice.get_mapped_range();

        let mut rgb = Vec::with_capacity((width * height * 3) as usize);
        for y in 0..height {
            let row = (y * padded) as usize;
            for x in 0..width {
                let i = row + (x * 4) as usize;
                rgb.extend_from_slice(&mapped[i..i + 3]);
            }
        }
        drop(mapped);
        staging.unmap();

        Image { width, height, rgb }
    }
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

pub(crate) fn write_png(path: &Path, image: &Image) -> Result<(PathBuf, u64, u32, u32), String> {
    let file =
        std::fs::File::create(path).map_err(|e| format!("create {}: {e}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), image.width, image.height);
    // What the header on `tip_table.rs` does: a generated file that says so in
    // itself, so somebody who finds it and starts editing is told before they
    // lose the edit. A PNG's text chunk is the only place a picture can hold
    // one. No version in it — that would churn every committed image at every
    // release for a fact the file does not need to carry.
    encoder
        .add_text_chunk("Software".to_owned(), "Umber".to_owned())
        .and_then(|()| {
            encoder.add_text_chunk(
                "Comment".to_owned(),
                "Generated from Umber's own interface code; do not edit. \
                 cargo run -p umber-app --example docs-images"
                    .to_owned(),
            )
        })
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    // These are flat interface colours with hard edges, which is the case the
    // filters were designed for; the slowest setting is still instant at this
    // size and takes a useful bite out of a file that is committed.
    encoder.set_compression(png::Compression::High);
    encoder
        .write_header()
        .and_then(|mut w| w.write_image_data(&image.rgb))
        .map_err(|e| format!("write {}: {e}", path.display()))?;

    let bytes = std::fs::metadata(path)
        .map(|m| m.len())
        .map_err(|e| format!("stat {}: {e}", path.display()))?;
    Ok((path.to_path_buf(), bytes, image.width, image.height))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dialog is 1000 wide and clamps to `available - 48`, and the field is
    /// picked so the clamp is exactly satisfied. If either number moves the
    /// picture silently gains a wide empty margin or loses the dialog's edge.
    #[test]
    fn the_settings_field_holds_the_dialog_at_full_size() {
        assert_eq!(SETTINGS_FIELD, vec2(1048.0, 688.0));
    }

    /// Every picture is a whole number of device pixels, so nothing lands on a
    /// half-pixel and comes back blurred.
    #[test]
    fn every_field_is_a_whole_number_of_pixels() {
        for (size, scale) in [
            (SETTINGS_FIELD, SETTINGS_SCALE),
            (PICKER_FIELD, PANEL_SCALE),
        ] {
            for edge in [size.x, size.y] {
                let px = edge * scale;
                assert_eq!(px, px.round(), "{edge} points is {px} pixels");
            }
        }
    }

    /// The trim is the only thing deciding how tall a picker picture is, so it
    /// has to find the content and it has to stop at the margin.
    #[test]
    fn trimming_cuts_back_to_the_last_thing_drawn() {
        let bg = Color32::from_rgb(1, 2, 3);
        let ink = Color32::from_rgb(9, 9, 9);
        let (w, h) = (4u32, 20u32);
        let mut rgb = Vec::new();
        for y in 0..h {
            for _ in 0..w {
                let c = if y == 5 { ink } else { bg };
                rgb.extend_from_slice(&[c.r(), c.g(), c.b()]);
            }
        }
        let image = Image {
            width: w,
            height: h,
            rgb,
        }
        .trim(bg, 3);
        assert_eq!(image.height, 9, "last ink row is 5, plus a margin of 3");
        assert_eq!(image.rgb.len(), (9 * w * 3) as usize);
    }

    /// A field with nothing in it must not truncate to nothing — an image of
    /// zero height is not a file any reader will open.
    #[test]
    fn trimming_an_empty_field_leaves_it_alone() {
        let bg = Color32::from_rgb(1, 2, 3);
        let (w, h) = (4u32, 6u32);
        let rgb = std::iter::repeat_n([bg.r(), bg.g(), bg.b()], (w * h) as usize)
            .flatten()
            .collect();
        let image = Image {
            width: w,
            height: h,
            rgb,
        }
        .trim(bg, 3);
        assert_eq!(image.height, h);
    }
}
