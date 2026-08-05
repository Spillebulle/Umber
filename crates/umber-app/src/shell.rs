//! A window of Umber's own, for a process that is not the editor.
//!
//! Umber is its own crash reporter and its own installer: the panic hook spawns
//! this executable with `--crash-report <path>`, and an update spawns it with
//! `--install-update <package>`. Both draw a box out of `theme`, `widgets`,
//! `icons` and `tabs::dialog_frame`, and both need exactly the same thing
//! underneath — a winit window, an adapter of their own, a surface, an egui
//! context — and nothing else. No canvas, no document, no `Editor`.
//!
//! This module is that underneath, and it exists because there were two of it.
//! The crash reporter had the only copy; the installer would have been the
//! second, which is the drift this codebase refuses everywhere the blend maths
//! is concerned and had no reason to allow here. A [`Page`] is the part that
//! differs, and it is the part with no wgpu in it at all — which is what makes
//! a page testable against `egui::Context::run_ui` without a device.
//!
//! ### What a page may assume
//!
//! * It is handed the whole window as one root `Ui`, exactly as `ui::draw` is.
//!   There is no central panel, because there is nothing to put beside the box:
//!   this window *is* the dialog.
//! * It is drawn under [`ControlFlow::Wait`], so a box being read costs nothing.
//!   A page that has something moving — a progress bar fed by a worker — says so
//!   through [`Page::poll`], and only then is a frame scheduled.
//! * Its `close` flag ends the window. Nothing else does, apart from the user
//!   closing it.

use crate::logo;
use crate::swapchain;
use crate::theme::{self, Palette};
use std::sync::Arc;
use std::time::{Duration, Instant};
use umber_render::Gpu;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

/// What one of these windows draws.
///
/// Deliberately free of wgpu and winit: everything a page decides can be
/// exercised by handing it an `egui::Ui`, which is how the crash box's wording
/// and the installer's stages are tested without a graphics device.
pub trait Page {
    /// The window's title.
    fn title(&self) -> String;

    /// Its size in logical points.
    ///
    /// Fixed rather than derived from the content, for the reason
    /// `metrics::BRUSH_LIBRARY` is fixed: a page whose body is unbounded — a
    /// backtrace, a log — would otherwise open taller than the screen with its
    /// buttons out of reach. Bodies scroll instead.
    fn size(&self) -> [f32; 2];

    /// The user's own theme. Read from their preferences by each page, because
    /// these processes have no `Editor` to take one from — which is exactly why
    /// prefs are a file rather than a field.
    fn palette(&self) -> Palette;

    /// Draw one frame. Setting `close` ends the window after it.
    fn draw(&mut self, ui: &mut egui::Ui, close: &mut bool);

    /// How long until this page wants drawing again, if it does.
    ///
    /// `None` — the default — is a page that only changes when the pointer or
    /// the keyboard touches it, which is the crash box and every dialog like
    /// it. A page waiting on a worker returns a delay, and only that page pays
    /// for the frames.
    fn poll(&mut self) -> Option<Duration> {
        None
    }
}

/// Show a page. Returns once its window has closed.
pub fn run(page: &mut dyn Page) -> Result<(), Box<dyn std::error::Error>> {
    // Before the first window, exactly as `lib::run` does it: the shell reads
    // the application identity when it creates the taskbar button, and a box
    // that appeared under a different identity would be a second Umber in the
    // taskbar. See `taskbar`.
    crate::taskbar::claim_identity();

    let event_loop = EventLoop::new()?;
    // Nothing animates unless a page says so, and then it says so per frame.
    event_loop.set_control_flow(ControlFlow::Wait);

    // The page's own palette, read once: `Page::palette` loads a file.
    let palette = page.palette();
    let mut host = Host {
        page,
        palette,
        gfx: None,
        failure: None,
    };
    event_loop.run_app(&mut host)?;

    match host.failure {
        Some(failure) => Err(failure.into()),
        None => Ok(()),
    }
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

struct Host<'a> {
    page: &'a mut dyn Page,
    palette: Palette,
    gfx: Option<Gfx>,
    /// Why the window could not be opened, if it could not. The caller falls
    /// back to whatever it can still do — the crash reporter prints the report
    /// to stderr rather than leaving somebody with nothing.
    failure: Option<String>,
}

impl ApplicationHandler for Host<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        match self.build(event_loop) {
            Ok(gfx) => self.gfx = Some(gfx),
            Err(e) => {
                // Nothing to draw with. Say so up the stack and leave.
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
        // would have seen it never happened.
        if gfx.egui_state.on_window_event(&gfx.window, &event).repaint {
            gfx.window.request_redraw();
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            // A zero on either axis skips the handler, as `app.rs`'s does, so
            // the surface keeps the configuration it had. Clamping to 1 was
            // enough to satisfy wgpu, which refuses a zero-area configure —
            // but `config` also sizes the `ScreenDescriptor`, so a zero
            // reported while a Wayland window is being mapped drew the whole
            // box into one pixel until the next real `Resized`.
            WindowEvent::Resized(size) => {
                if size.width > 0 && size.height > 0 {
                    gfx.config.width = size.width;
                    gfx.config.height = size.height;
                    gfx.surface.configure(&gfx.gpu.device, &gfx.config);
                    gfx.window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                // Named rather than tested inline. Clippy would rather this
                // were a match guard — `RedrawRequested if self.render()` —
                // which hides a frame being drawn inside a pattern, and a guard
                // with a side effect is a worse thing to read than an extra
                // line.
                let dismissed = self.render(event_loop);
                if dismissed {
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }
}

impl Host<'_> {
    /// Window, device, surface and egui.
    ///
    /// Every step returns an error rather than panicking. A panic in the crash
    /// reporter would be a panic inside a panic handler's own child — legible
    /// on stderr, but the last thing a person needs to read — and a panic in
    /// the installer would leave somebody mid-update with no window.
    fn build(&self, event_loop: &ActiveEventLoop) -> Result<Gfx, String> {
        let size = self.page.size();
        let attrs = Window::default_attributes()
            .with_title(self.page.title())
            .with_window_icon(logo::window_icon())
            .with_resizable(true)
            .with_inner_size(winit::dpi::LogicalSize::new(size[0], size[1]));

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

    /// Draw one frame. Answers true when the page has asked to close.
    fn render(&mut self, event_loop: &ActiveEventLoop) -> bool {
        let Some(gfx) = self.gfx.as_mut() else {
            return false;
        };

        // Asked before the frame is built, so anything a worker reported since
        // the last one is on screen in *this* frame rather than the next.
        let again = self.page.poll();

        let raw_input = gfx.egui_state.take_egui_input(&gfx.window);
        let palette = self.palette;
        let page = &mut *self.page;
        let mut close = false;

        // `run_ui` hands over the whole window as one root `Ui`, exactly as
        // `ui::draw` is given one.
        let output = gfx.egui_ctx.run_ui(raw_input, |ui| {
            page.draw(ui, &mut close);
        });

        gfx.egui_state
            .handle_platform_output(&gfx.window, output.platform_output);
        let pixels_per_point = gfx.egui_ctx.pixels_per_point();
        let paint_jobs = gfx.egui_ctx.tessellate(output.shapes, pixels_per_point);

        // Uploaded before the surface is acquired, for the reason `app::render`
        // states at the same point and which applies here with more force: the
        // acquisition below can decide not to draw, and a skipped frame that
        // dropped `textures_delta.set` would leave `egui_wgpu` holding a
        // partial update for a texture it never allocated — met with
        // `.expect("Tried to update a texture that has not been allocated
        // yet.")`. That is a panic inside the process whose whole job is to
        // survive one, which is the double fault this window exists to avoid.
        for (id, delta) in &output.textures_delta.set {
            gfx.egui_renderer
                .update_texture(&gfx.gpu.device, &gfx.gpu.queue, *id, delta);
        }

        // The same decision `app::render` makes, through the same model rather
        // than through a second copy of its arms. `swapchain` has no wgpu
        // lifetime state in it, so it is the cheapest thing in the application
        // to share, and sharing it puts the second surface in Umber under the
        // one tested rule.
        //
        // This host takes `reconfigure_now` and deliberately ignores
        // `reconfigure_later`: it keeps no state between frames, and a
        // suboptimal swapchain here is a box drawn slightly the wrong size for
        // one frame rather than a canvas mid-resize. `Resized` reconfigures,
        // and for these windows that is enough.
        let (acquisition, texture) = match gfx.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => (swapchain::Acquisition::Fresh, Some(t)),
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => {
                (swapchain::Acquisition::Suboptimal, Some(t))
            }
            wgpu::CurrentSurfaceTexture::Outdated => (swapchain::Acquisition::Outdated, None),
            wgpu::CurrentSurfaceTexture::Lost => (swapchain::Acquisition::Lost, None),
            wgpu::CurrentSurfaceTexture::Occluded => (swapchain::Acquisition::Occluded, None),
            wgpu::CurrentSurfaceTexture::Timeout => (swapchain::Acquisition::Timeout, None),
            _ => (swapchain::Acquisition::Failed, None),
        };
        debug_assert_eq!(acquisition.carries_texture(), texture.is_some());
        let plan = swapchain::plan(acquisition);
        // Let go of anything not being drawn into before touching the surface,
        // in that order and for the reason `app::render` gives at the same
        // point.
        let acquired = texture.filter(|_| plan.draws());
        if plan.reconfigure_now() {
            gfx.surface.configure(&gfx.gpu.device, &gfx.config);
        }
        let Some(frame) = acquired else {
            // Nothing was recorded, so the frees may be applied at once — the
            // one case where that is true, exactly as in `app::render`.
            crate::app::release_finished_textures(
                &mut gfx.egui_renderer,
                &output.textures_delta.free,
            );
            // And ask for the frame that will draw on whatever was just put
            // right. `app::render` needs no such line because a canvas gives
            // itself redraws for other reasons; this window gives itself none
            // and sits under `ControlFlow::Wait`, so a skipped frame would
            // leave the box unpainted until the user happened to move the
            // mouse — on the one window where nothing on screen is the whole
            // failure. Costs one frame, and only on a skip.
            gfx.window.request_redraw();
            return close;
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = gfx
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("umber-shell"),
            });

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
                label: Some("umber-shell"),
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
        // Submit, and only then give egui's finished textures back — through
        // `app::submit_frame`, which is the one place that does both so the two
        // cannot be put the wrong way round. This used to free first, which is
        // the ordering that makes a same-frame free a wgpu validation error at
        // submit: `free_texture` calls `Texture::destroy`, and that takes
        // effect immediately rather than when the last reference goes. wgpu's
        // default handler turns it into a panic, and these processes install no
        // `on_uncaptured_error` — so it would have been a panic while reporting
        // a panic.
        crate::app::submit_frame(
            &gfx.gpu,
            &mut gfx.egui_renderer,
            encoder,
            &output.textures_delta.free,
        );
        frame.present();

        if output
            .viewport_output
            .values()
            .any(|v| v.repaint_delay.is_zero())
        {
            gfx.window.request_redraw();
        }
        // A page with something moving asks for the next frame here rather than
        // through egui, because what changed is outside egui entirely: a worker
        // thread's progress. `WaitUntil` rather than `Poll`, so a page that
        // wants a frame every eighth of a second costs eight frames a second
        // and not four hundred.
        if let Some(delay) = again {
            event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + delay));
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
