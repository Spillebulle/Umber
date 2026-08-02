//! The crash box: a window of its own, in a process of its own.
//!
//! This runs after Umber has died, started by [`super::spawn_reporter`] with
//! `--crash-report <path>`. It is the same executable, so it has `theme`,
//! `widgets`, `icons` and `tabs::dialog_frame` — the box is built out of the
//! same pieces as the settings dialog and the module library, and looks like
//! them because it *is* them. What it does not share is the device: this
//! process asks for its own adapter, its own surface and its own egui context,
//! which is the whole reason the reporter is a second process at all. See the
//! module docs on [`super`].
//!
//! There is deliberately **no canvas**. `CanvasRenderer` is never built, no
//! document is loaded, and nothing here touches `umber-core` beyond what the
//! report already holds — so the crash box cannot be stopped by the same thing
//! that stopped Umber.
//!
//! ### Why there is no "Copy details" button
//!
//! `egui-winit` is built with `default-features = false` (see the crate's
//! `Cargo.toml`, and `about::link_row` for the same problem with hyperlinks),
//! so its `clipboard` feature is not compiled in and `Context::copy_text` is a
//! no-op. A button that looks like it copies and does nothing is the control
//! that lies, which this codebase refuses everywhere else; turning the feature
//! on means `arboard`, which on Linux is a new linked dependency that
//! `packaging/linux/build-packages.sh` and the `PKGBUILD` would have to declare
//! by hand — a real packaging change for one button.
//!
//! So the report gets out of the window a better way: it is already a file.
//! The box names the path and offers to open the folder, and the details
//! themselves are a read-only `TextEdit`, which is genuinely selectable. The
//! whole report can be sent; nobody has to retype a backtrace.

use crate::icons::{self, Icon};
use crate::logo;
use crate::prefs;
use crate::tabs;
use crate::theme::{self, Palette, metrics, text};
use egui::{Sense, vec2};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use umber_render::Gpu;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use super::report::Report;

/// The window, in logical points.
///
/// Fixed rather than derived from the content, for the reason
/// `metrics::BRUSH_LIBRARY` is fixed: the details block is a backtrace, whose
/// length is unbounded, and a window that grew to fit one would be taller than
/// the screen with its buttons out of reach. The details scroll instead.
const WINDOW: [f32; 2] = [560.0, 520.0];

/// What the footer takes: one row of buttons and the space above it.
const FOOTER: f32 = 36.0;

/// The least the scrolling body is ever given, however short the window is
/// dragged. Below this it stops being readable and the box may as well not have
/// opened.
const MIN_BODY: f32 = 120.0;

/// Show the report. Returns once the window has closed.
pub fn show(report: &Report, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    // Before the first window, exactly as `run` does it: the shell reads the
    // application identity when it creates the taskbar button, and a crash box
    // that appeared under a different identity would be a second Umber in the
    // taskbar. See `taskbar`.
    crate::taskbar::claim_identity();

    let event_loop = EventLoop::new()?;
    // Nothing here animates, so the box should cost nothing while it is read.
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut reporter = Reporter::new(report, path);
    event_loop.run_app(&mut reporter)?;

    if let Some(failure) = reporter.failure {
        return Err(failure.into());
    }
    if reporter.restart {
        super::restart();
    }
    Ok(())
}

/// Everything tied to a live window, as in `app::Graphics` and for the same
/// reason: it does not exist until `resumed`.
struct Gfx {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    gpu: Gpu,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
}

struct Reporter<'a> {
    report: &'a Report,
    /// Built once. It is the same text the file holds and the same text the
    /// details block shows — assembled in `report.rs` so those cannot drift.
    details: String,
    report_path: PathBuf,
    palette: Palette,
    gfx: Option<Gfx>,
    /// "Technical details", closed to begin with. A crash box that opens on a
    /// page of frame addresses buries the sentence about somebody's work.
    expanded: bool,
    restart: bool,
    /// Why the window could not be opened, if it could not. The caller prints
    /// the report to stderr rather than leaving somebody with nothing.
    failure: Option<String>,
}

impl<'a> Reporter<'a> {
    fn new(report: &'a Report, path: &Path) -> Self {
        // The user's own theme, read from their preferences file. A crash box
        // in the wrong theme is a small thing and an avoidable one — and this
        // process has no editor to take it from, which is exactly why prefs are
        // a file rather than a field.
        let prefs = prefs::load();
        Self {
            report,
            details: report.details(),
            report_path: path.to_path_buf(),
            palette: Palette::with_accent(prefs.theme, prefs.accent),
            gfx: None,
            expanded: false,
            restart: false,
            failure: None,
        }
    }
}

impl ApplicationHandler for Reporter<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        match self.build(event_loop) {
            Ok(gfx) => self.gfx = Some(gfx),
            Err(e) => {
                // Nothing to draw with. Say so up the stack and leave; the
                // caller falls back to stderr, which is the one output that
                // cannot fail.
                self.failure = Some(e);
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(gfx) = self.gfx.as_mut() else { return };
        // The answer matters, and forgetting it is a bug this had: under
        // `ControlFlow::Wait` nothing draws unless a redraw is asked for, and a
        // frame is the only thing that ever *reads* the input egui was just
        // handed. Without this the pointer moved, the button went down and up,
        // and the box sat there — every click swallowed, because the frame that
        // would have seen it never happened. `app.rs` asks for its redraws from
        // the input path for exactly this reason; there is no canvas here to
        // hide it.
        if gfx.egui_state.on_window_event(&gfx.window, &event).repaint {
            gfx.window.request_redraw();
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                gfx.config.width = size.width.max(1);
                gfx.config.height = size.height.max(1);
                gfx.surface.configure(&gfx.gpu.device, &gfx.config);
                gfx.window.request_redraw();
            }
            WindowEvent::RedrawRequested => {
                // Named rather than tested inline. Clippy would rather this
                // were a match guard — `RedrawRequested if self.render()` —
                // which hides a frame being drawn inside a pattern, and a guard
                // with a side effect is a worse thing to read than an extra
                // line.
                let dismissed = self.render();
                if dismissed {
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }
}

impl Reporter<'_> {
    /// Window, device, surface and egui.
    ///
    /// Every step returns an error rather than panicking. A panic in the crash
    /// reporter would be a panic inside a panic handler's own child — legible
    /// on stderr, but the last thing a person needs to read.
    fn build(&self, event_loop: &ActiveEventLoop) -> Result<Gfx, String> {
        let attrs = Window::default_attributes()
            .with_title("Umber — crash report")
            .with_window_icon(logo::window_icon())
            .with_resizable(true)
            .with_inner_size(winit::dpi::LogicalSize::new(WINDOW[0], WINDOW[1]));

        // Both of these are the same platform rules `app::resumed` documents:
        // Windows draws the taskbar button from a second icon, and Linux
        // matches an installed `.desktop` file by name rather than using the
        // window's own icon at all.
        #[cfg(target_os = "windows")]
        let attrs = {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs.with_taskbar_icon(logo::taskbar_icon())
        };
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
        let attrs = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            use winit::platform::x11::WindowAttributesExtX11;
            let attrs = WindowAttributesExtWayland::with_name(attrs, crate::taskbar::APP_ID, "");
            WindowAttributesExtX11::with_name(attrs, "umber", "umber")
        };

        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .map_err(|e| format!("could not create the window: {e}"))?,
        );

        let instance = Gpu::create_instance();
        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| format!("could not create a surface: {e}"))?;
        let gpu = pollster::block_on(Gpu::new(instance, Some(&surface)))?;

        let size = window.inner_size();
        let config = gpu.surface_config(&surface, size.width, size.height);
        surface.configure(&gpu.device, &config);

        let egui_ctx = egui::Context::default();
        theme::install_fonts(&egui_ctx);
        theme::apply(&egui_ctx, &self.palette);
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui_ctx.viewport_id(),
            &window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        let egui_renderer = egui_wgpu::Renderer::new(
            &gpu.device,
            config.format,
            egui_wgpu::RendererOptions::default(),
        );

        window.request_redraw();
        Ok(Gfx {
            window,
            surface,
            config,
            gpu,
            egui_ctx,
            egui_state,
            egui_renderer,
        })
    }

    /// Draw one frame. Answers true when the box has been dismissed.
    fn render(&mut self) -> bool {
        let Some(gfx) = self.gfx.as_mut() else {
            return false;
        };

        let raw_input = gfx.egui_state.take_egui_input(&gfx.window);
        let palette = self.palette;
        let report = self.report;
        let details = &self.details;
        let report_path = &self.report_path;
        let expanded = &mut self.expanded;
        let restart = &mut self.restart;
        let mut close = false;

        // `run_ui` hands over the whole window as one root `Ui`, exactly as
        // `ui::draw` is given one. There is no central panel here because there
        // is nothing to put beside the box: this window *is* the dialog.
        let output = gfx.egui_ctx.run_ui(raw_input, |ui| {
            body(
                ui,
                &palette,
                report,
                details,
                report_path,
                Actions {
                    expanded,
                    restart: &mut *restart,
                    close: &mut close,
                },
            );
        });

        gfx.egui_state
            .handle_platform_output(&gfx.window, output.platform_output);
        let pixels_per_point = gfx.egui_ctx.pixels_per_point();
        let paint_jobs = gfx.egui_ctx.tessellate(output.shapes, pixels_per_point);

        // The same arms `app::render` takes, minus the logging: a box that
        // misses one frame is redrawn by the next event and nothing is lost in
        // the meantime, so anything but a usable texture simply skips.
        let acquired = match gfx.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => Some(t),
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => Some(t),
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                gfx.surface.configure(&gfx.gpu.device, &gfx.config);
                None
            }
            _ => None,
        };
        let Some(frame) = acquired else {
            return close;
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = gfx
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("crash-report"),
            });

        for (id, delta) in &output.textures_delta.set {
            gfx.egui_renderer
                .update_texture(&gfx.gpu.device, &gfx.gpu.queue, *id, delta);
        }
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [gfx.config.width, gfx.config.height],
            pixels_per_point,
        };
        gfx.egui_renderer.update_buffers(
            &gfx.gpu.device,
            &gfx.gpu.queue,
            &mut encoder,
            &paint_jobs,
            &screen,
        );
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("crash-report"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        // Cleared rather than loaded: there is no canvas under
                        // this, and the first frame of an unconfigured surface
                        // otherwise shows whatever the allocation held.
                        load: wgpu::LoadOp::Clear(clear_colour(&palette)),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            gfx.egui_renderer
                .render(&mut pass.forget_lifetime(), &paint_jobs, &screen);
        }
        for id in &output.textures_delta.free {
            gfx.egui_renderer.free_texture(id);
        }
        gfx.gpu.queue.submit(Some(encoder.finish()));
        frame.present();

        if output
            .viewport_output
            .values()
            .any(|v| v.repaint_delay.is_zero())
        {
            gfx.window.request_redraw();
        }
        close
    }
}

/// The backdrop, as the surface wants it.
///
/// The surface is deliberately **non**-sRGB — `Gpu::surface_config` picks one,
/// for the reason `composite.wgsl` does its own encode — so the palette's
/// already-encoded bytes go straight through with no conversion. Passing them
/// through `srgb_to_linear` would wash the box out, which is the same trap
/// documented under "Colour space".
fn clear_colour(p: &Palette) -> wgpu::Color {
    wgpu::Color {
        r: p.backdrop.r() as f64 / 255.0,
        g: p.backdrop.g() as f64 / 255.0,
        b: p.backdrop.b() as f64 / 255.0,
        a: 1.0,
    }
}

/// What the body may change. A struct rather than four `&mut` parameters
/// because a run of bare booleans in a signature is how the wrong one gets set.
struct Actions<'a> {
    expanded: &'a mut bool,
    restart: &'a mut bool,
    close: &'a mut bool,
}

/// The box itself.
fn body(
    ui: &mut egui::Ui,
    p: &Palette,
    report: &Report,
    details: &str,
    report_path: &Path,
    actions: Actions<'_>,
) {
    // The dialog frame every modal in the application uses, inset far enough
    // for the backdrop to show around it — so this reads as the same object as
    // the settings dialog and the module library rather than as a page of text
    // that happens to be in a window.
    let inset = egui::Frame::NONE.inner_margin(egui::Margin::same(10));
    inset.show(ui, |ui| {
        tabs::dialog_frame(p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(ui.available_height());

            heading(ui, p, report);
            ui.add_space(12.0);

            // The settings dialog's shape, and for its reason: a header, **one**
            // vertical `ScrollArea` claiming an explicit height, a footer. Two
            // things here are unbounded — the list of open documents and the
            // backtrace — and letting either size the window means a box whose
            // buttons are off the bottom of the screen. Expanding the details
            // used to do exactly that, which is what this replaces.
            //
            // The height is what is left after the footer, taken before the
            // body is drawn rather than after, so the buttons cannot be pushed
            // anywhere by what goes above them.
            let room = (ui.available_height() - FOOTER).max(MIN_BODY);
            egui::ScrollArea::vertical()
                .max_height(room)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // What happened, in plain words. The heading is the
                    // greeting; this is the part somebody has to act on.
                    paragraph(
                        ui,
                        p,
                        "Umber ran into a problem it could not carry on from and \
                         had to stop. Nothing about this has left your machine: \
                         the report below is a file on this computer and stays \
                         there unless you send it.",
                    );

                    ui.add_space(14.0);
                    work(ui, p, report);

                    ui.add_space(14.0);
                    technical(ui, p, details, actions.expanded);

                    ui.add_space(12.0);
                    where_the_report_is(ui, p, report_path);
                });

            // The footer, outside the scroll area, so the buttons stay where
            // they are however long the details run — a control that walks out
            // from under the pointer as the thing above it grows is how a Close
            // becomes a Restart.
            ui.add_space(10.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if tabs::button(ui, p, "Restart Umber", true) {
                    *actions.restart = true;
                    *actions.close = true;
                }
                if tabs::button(ui, p, "Close", false) {
                    *actions.close = true;
                }
            });
        });
    });
}

/// The mark, the heading and the one line of fact under it.
///
/// The heading is the application's own voice and is exactly what it says. The
/// line under it carries the version, because a crash box that does not say
/// which build broke is a crash box nobody can act on.
fn heading(ui: &mut egui::Ui, p: &Palette, report: &Report) {
    ui.horizontal(|ui| {
        let (mark, _) = ui.allocate_exact_size(egui::Vec2::splat(26.0), Sense::hover());
        icons::draw(ui.painter(), mark, Icon::Alert, p.warning);
        ui.add_space(10.0);
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new("Oh no, oopsy")
                    .size(text::HEADING)
                    .color(p.text_strong)
                    .strong(),
            );
            let version = if report.version.is_empty() {
                "Umber stopped unexpectedly".to_string()
            } else {
                format!("Umber {} stopped unexpectedly", report.version)
            };
            ui.label(
                egui::RichText::new(version)
                    .size(text::SMALL)
                    .color(p.text_muted),
            );
        });
    });
}

/// What happened to the artist's work — the single most useful thing this box
/// can contain, and the one that must never overstate.
///
/// Every sentence comes from `report.rs`, where the rule about a copy being
/// complete is decided and tested. Nothing is phrased here.
fn work(ui: &mut egui::Ui, p: &Palette, report: &Report) {
    let rescued = report.rescued();
    let at_risk = report.at_risk();
    if rescued.is_empty() && at_risk.is_empty() {
        // Not silence, and not a claim either. "Nothing was open with unsaved
        // work" is a fact; "your work is safe" would be a promise about files
        // this process has not looked at.
        section(ui, p, "Your work");
        note(ui, p, "Nothing was open with unsaved changes.");
        return;
    }

    section(ui, p, "Your work");
    for rescue in &rescued {
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(&rescue.title)
                .size(text::SMALL)
                .color(p.text_strong),
        );
        path_line(ui, p, &rescue.path);
        note(ui, p, &rescue.note());
    }
    for title in &at_risk {
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(title)
                .size(text::SMALL)
                .color(p.text_strong),
        );
        note(ui, p, "Umber had no copy of this one.");
    }
}

/// The collapsed technical section.
///
/// The header is allocated unconditionally and hit-tested by geometry, which is
/// the rule "a widget revealed on hover must not be what decides the hover"
/// exists for — although here it is a plain click target, so the only thing at
/// stake is a chevron that does not flicker.
fn technical(ui: &mut egui::Ui, p: &Palette, details: &str, expanded: &mut bool) {
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), metrics::DROPDOWN),
        Sense::click(),
    );
    let painter = ui.painter();
    let chevron = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 7.0, rect.center().y),
        egui::Vec2::splat(12.0),
    );
    let ink = if response.hovered() {
        p.text_strong
    } else {
        p.text_muted
    };
    icons::draw(
        painter,
        chevron,
        if *expanded {
            Icon::ChevronUp
        } else {
            Icon::ChevronDown
        },
        ink,
    );
    painter.text(
        egui::pos2(rect.left() + 18.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        "Technical details",
        egui::FontId::proportional(text::SMALL),
        ink,
    );
    if response.clicked() {
        *expanded = !*expanded;
    }
    if !*expanded {
        return;
    }

    ui.add_space(6.0);
    // Deliberately **no scroll area of its own**. This sits inside the body's
    // one `ScrollArea`, and a second one nested in it would make the wheel mean
    // two things depending on where the pointer happened to be — the rule the
    // settings dialog's panes live by. The text is bounded instead, by
    // `report::BACKTRACE_LIMIT` where it is captured, so "as tall as it needs"
    // has a ceiling.
    //
    // Read-only, and therefore genuinely selectable: `&str` is a `TextBuffer`
    // that reports itself immutable, so egui draws a real text field that
    // cannot be edited. See the module docs for why there is no Copy button
    // beside it.
    let mut text = details;
    ui.add(
        egui::TextEdit::multiline(&mut text)
            .font(egui::FontId::monospace(text::TINY))
            .desired_width(f32::INFINITY)
            .text_color(p.text_muted),
    );
}

/// Where the file is, and the one control that gets it out of this window.
///
/// The path is on a line of its own, **wrapped explicitly**. A report path is
/// the longest string in this window — the data directory plus a timestamped
/// name — and an egui label defaults to `TextWrapMode::Extend`, which is what
/// put the brush browser wider than the screen. Beside a button in a
/// right-to-left row it does not widen the window, because the window is fixed;
/// it runs off the *left* edge instead and the start of the path is lost, which
/// is worse. `Label::wrap` is the fix, not `set_max_width`.
///
/// The button sits under it and to the left, with the dialog's own actions far
/// away at the bottom right: this one belongs to the path above it, not to the
/// question the box is asking.
fn where_the_report_is(ui: &mut egui::Ui, p: &Palette, path: &Path) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(format!("Saved to {}", path.display()))
                .size(9.5)
                .color(p.text_dim)
                .line_height(Some(12.5)),
        )
        .wrap(),
    );
    ui.add_space(6.0);
    if let Some(dir) = path.parent()
        && tabs::button(ui, p, "Show the report", false)
    {
        // The same opener the settings dialog uses for the autosave directory.
        // Best effort by construction — there is no portable way to ask — which
        // is why the path is printed above it rather than only linked.
        if let Err(e) = crate::autosave::reveal(dir) {
            log::warn!("could not open {}: {e}", dir.display());
        }
    }
}

// ---------------------------------------------------------------------------
// Small pieces, following `about.rs`'s
// ---------------------------------------------------------------------------

fn section(ui: &mut egui::Ui, p: &Palette, title: &str) {
    ui.label(
        egui::RichText::new(title)
            .size(text::SMALL)
            .color(p.text_dim)
            .strong(),
    );
}

fn paragraph(ui: &mut egui::Ui, p: &Palette, message: &str) {
    ui.label(
        egui::RichText::new(message)
            .size(text::SMALL)
            .color(p.text)
            .line_height(Some(15.0)),
    );
}

fn note(ui: &mut egui::Ui, p: &Palette, message: &str) {
    ui.label(
        egui::RichText::new(message)
            .size(10.0)
            .color(p.text_dim)
            .line_height(Some(13.5)),
    );
}

/// A path, in the muted ink the rest of the interface gives one.
fn path_line(ui: &mut egui::Ui, p: &Palette, path: &str) {
    ui.label(
        egui::RichText::new(path)
            .size(10.0)
            .color(p.accent)
            .line_height(Some(13.5)),
    );
}
