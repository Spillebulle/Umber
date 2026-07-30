//! Window lifecycle, input translation and the frame loop.

use crate::editor::{Editor, Interaction};
use crate::ui;
use glam::{UVec2, Vec2};
use std::sync::Arc;
use umber_core::{Brush, BrushMode, Camera, Dab, InputPoint, PixelPatch};
use umber_render::{CanvasRenderer, CompositeParams, Gpu};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{Window, WindowId};

/// Everything tied to a live window and GPU surface.
///
/// Kept in an `Option` on [`UmberApp`] because Android destroys and recreates
/// the surface across suspend/resume while the editor state must survive.
struct Graphics {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    gpu: Gpu,
    canvas: CanvasRenderer,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
}

#[derive(Default)]
pub struct UmberApp {
    gfx: Option<Graphics>,
    editor: Editor,
    modifiers: ModifiersState,
    last_frame: Option<std::time::Instant>,
}

impl UmberApp {
    fn viewport(&self) -> Vec2 {
        match &self.gfx {
            Some(g) => Vec2::new(g.config.width as f32, g.config.height as f32),
            None => Vec2::ONE,
        }
    }

    /// Finish the current stroke: capture undo state, bake it into the layer.
    ///
    /// The layer is untouched until this point, so reading it here captures
    /// exactly the pre-stroke pixels the undo stack needs.
    fn finish_stroke(&mut self) {
        let Some(gfx) = self.gfx.as_mut() else { return };
        if !self.editor.stroke.is_active() {
            return;
        }

        let bounds = self.editor.stroke.bounds();
        self.editor.stroke.end();
        self.editor.interaction = Interaction::Idle;

        let mut enc = gfx
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("finish-stroke"),
            });

        // Dabs produced since the last frame are still queued — pointer events
        // arrive far faster than frames. They have to reach the scratch texture
        // before it is committed, or the tail of the stroke is left behind: it
        // reappears as a live preview on the next frame (the stroke "hangs"),
        // and then gets baked in by the *next* stroke's commit, wearing that
        // stroke's colour and opacity.
        let tail: Vec<Dab> = self.editor.stroke.drain_pending().collect();
        if !tail.is_empty() {
            gfx.canvas.begin_frame();
            gfx.canvas.draw_dabs(&gfx.gpu.queue, &mut enc, &tail);
        }

        let Some(rect) = bounds.to_pixels_clamped(self.editor.doc.size) else {
            // Stroke fell entirely outside the canvas — nothing to commit, but
            // the scratch surface may still hold dabs.
            gfx.canvas.clear_stroke(&mut enc);
            gfx.gpu.queue.submit(Some(enc.finish()));
            return;
        };

        let slot = self.editor.stroke_slot;

        // Capture undo state first. `read_layer_rect` submits and blocks on its
        // own encoder, so it observes the layer before `enc` commits anything.
        let before = gfx
            .canvas
            .read_layer_rect(&gfx.gpu.device, &gfx.gpu.queue, slot, rect);
        self.editor
            .history
            .record(PixelPatch::new(rect, slot, before));

        gfx.canvas.commit_stroke(
            &gfx.gpu.queue,
            &mut enc,
            slot,
            rect,
            self.editor.stroke_style,
        );
        gfx.gpu.queue.submit(Some(enc.finish()));
    }

    /// Throw the in-progress stroke away without touching the layer.
    ///
    /// Used when a gesture turns out not to be a stroke — a second finger
    /// landing means the user meant to pinch, and the stray dab from the first
    /// finger should never reach the canvas or the undo stack.
    fn cancel_stroke(&mut self) {
        let Some(gfx) = self.gfx.as_mut() else { return };
        if !self.editor.stroke.is_active() {
            return;
        }
        self.editor.stroke.end();
        // Unlike a normal finish, these are dropped rather than flushed — the
        // whole point is that nothing from this gesture reaches the canvas.
        self.editor.stroke.clear_pending();
        self.editor.interaction = Interaction::Idle;

        let mut enc = gfx
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("cancel-stroke"),
            });
        gfx.canvas.clear_stroke(&mut enc);
        gfx.gpu.queue.submit(Some(enc.finish()));
    }

    /// Begin a stroke, guaranteeing the scratch surface starts empty.
    ///
    /// Every path that ends a stroke already clears the scratch, so this is
    /// belt-and-braces — but stale coverage leaking into a new stroke is
    /// precisely the failure this module has already been bitten by, and it
    /// presents as a mystery colour change rather than as anything obvious.
    fn start_stroke(&mut self, point: InputPoint) {
        self.editor.begin_stroke(point);

        if let Some(gfx) = self.gfx.as_ref() {
            let mut enc = gfx
                .gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("begin-stroke"),
                });
            gfx.canvas.clear_stroke(&mut enc);
            gfx.gpu.queue.submit(Some(enc.finish()));
        }
    }

    fn undo(&mut self) {
        let Some(gfx) = self.gfx.as_mut() else { return };
        let Some(patch) = self.editor.history.take_undo() else {
            return;
        };
        let current =
            gfx.canvas
                .read_layer_rect(&gfx.gpu.device, &gfx.gpu.queue, patch.slot, patch.rect);
        gfx.canvas
            .write_layer_rect(&gfx.gpu.queue, patch.slot, patch.rect, &patch.bytes);
        self.editor
            .history
            .push_redo(PixelPatch::new(patch.rect, patch.slot, current));
    }

    fn redo(&mut self) {
        let Some(gfx) = self.gfx.as_mut() else { return };
        let Some(patch) = self.editor.history.take_redo() else {
            return;
        };
        let current =
            gfx.canvas
                .read_layer_rect(&gfx.gpu.device, &gfx.gpu.queue, patch.slot, patch.rect);
        gfx.canvas
            .write_layer_rect(&gfx.gpu.queue, patch.slot, patch.rect, &patch.bytes);
        self.editor
            .history
            .push_undo(PixelPatch::new(patch.rect, patch.slot, current));
    }

    /// Erase the active layer, leaving the rest of the stack alone.
    fn clear_active_layer(&mut self) {
        let slot = self.editor.layers.active_slot();
        let Some(gfx) = self.gfx.as_mut() else { return };
        let mut enc = gfx
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("clear"),
            });
        gfx.canvas.clear_layer(&mut enc, slot);
        gfx.canvas.clear_stroke(&mut enc);
        gfx.gpu.queue.submit(Some(enc.finish()));
        // Undo entries reference pixels that no longer exist in any meaningful
        // sense; keeping them would let undo resurrect part of a cleared layer.
        self.editor.history.clear();
    }

    fn add_layer(&mut self) {
        let Some(slot) = self.editor.layers.add() else {
            log::warn!("layer limit reached");
            return;
        };
        let needed = self.editor.layers.slot_capacity_needed();

        let Some(gfx) = self.gfx.as_mut() else { return };
        gfx.canvas
            .ensure_slots(&gfx.gpu.device, &gfx.gpu.queue, needed);

        // A recycled slot still holds the deleted layer's pixels.
        let mut enc = gfx
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("init-layer"),
            });
        gfx.canvas.clear_layer(&mut enc, slot);
        gfx.gpu.queue.submit(Some(enc.finish()));
    }

    fn delete_layer(&mut self, index: usize) {
        if self.editor.layers.remove(index).is_none() {
            return;
        }
        // Slots are recycled, so an undo entry recorded against the freed slot
        // would later be replayed into whichever layer inherits it. Dropping
        // history is the blunt but safe fix; structural undo is the real one.
        self.editor.history.clear();
    }

    fn handle_keys(&mut self, key: KeyCode, pressed: bool) -> bool {
        if key == KeyCode::Space {
            self.editor.space_down = pressed;
            return true;
        }
        if !pressed {
            return false;
        }

        let ctrl = self.modifiers.control_key() || self.modifiers.super_key();
        let shift = self.modifiers.shift_key();

        match key {
            KeyCode::KeyZ if ctrl && shift => self.redo(),
            KeyCode::KeyZ if ctrl => self.undo(),
            KeyCode::KeyY if ctrl => self.redo(),
            KeyCode::Digit0 if ctrl => {
                self.editor.camera = Camera::fit(self.editor.doc.size_vec2(), self.viewport());
            }
            KeyCode::Digit1 if ctrl => self.editor.camera.zoom = 1.0,
            KeyCode::KeyB => self.editor.brush.mode = BrushMode::Paint,
            KeyCode::KeyE => self.editor.brush.mode = BrushMode::Erase,
            KeyCode::BracketLeft => {
                self.editor.brush.size =
                    (self.editor.brush.size / 1.15).clamp(Brush::MIN_SIZE, Brush::MAX_SIZE);
            }
            KeyCode::BracketRight => {
                self.editor.brush.size =
                    (self.editor.brush.size * 1.15).clamp(Brush::MIN_SIZE, Brush::MAX_SIZE);
            }
            _ => return false,
        }
        true
    }

    /// Two-finger pinch: pan by the midpoint delta, zoom by the spread ratio.
    fn update_pinch(&mut self) {
        let viewport = self.viewport();
        let pts: Vec<Vec2> = self.editor.touches.values().copied().collect();
        if pts.len() != 2 {
            self.editor.pinch = None;
            return;
        }
        let mid = (pts[0] + pts[1]) * 0.5;
        let dist = (pts[0] - pts[1]).length();

        if let Some((prev_dist, prev_mid)) = self.editor.pinch {
            self.editor.camera.pan_by_screen(mid - prev_mid);
            if prev_dist > 1.0 && dist > 1.0 {
                let factor = dist / prev_dist;
                self.editor.camera.zoom_at(mid, factor, viewport);
            }
        }
        self.editor.pinch = Some((dist, mid));
    }

    fn render(&mut self) {
        let Some(gfx) = self.gfx.as_mut() else { return };

        let now = std::time::Instant::now();
        if let Some(prev) = self.last_frame {
            self.editor.record_frame_time((now - prev).as_secs_f32());
        }
        self.last_frame = Some(now);

        // --- UI ---
        // `Context::run` discards the closure's return value, so the panel's
        // actions are captured out of it instead.
        let editor = &mut self.editor;
        let mut actions = ui::UiActions::default();
        let raw_input = gfx.egui_state.take_egui_input(&gfx.window);
        let full_output = gfx.egui_ctx.run_ui(raw_input, |ui| {
            actions = ui::draw(ui, editor);
        });

        let egui::FullOutput {
            platform_output,
            textures_delta,
            shapes,
            pixels_per_point,
            ..
        } = full_output;
        gfx.egui_state
            .handle_platform_output(&gfx.window, platform_output);

        let surface_texture = match gfx.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => t,
            // Suboptimal still gives a usable texture; reconfiguring is a
            // next-frame concern, so draw this one rather than dropping it.
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => {
                gfx.surface.configure(&gfx.gpu.device, &gfx.config);
                t
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                gfx.surface.configure(&gfx.gpu.device, &gfx.config);
                return;
            }
            // Minimised or hidden — skip the frame entirely.
            wgpu::CurrentSurfaceTexture::Occluded | wgpu::CurrentSurfaceTexture::Timeout => return,
            other => {
                log::warn!("could not acquire surface texture: {other:?}");
                return;
            }
        };
        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = gfx
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });

        // --- canvas ---
        gfx.canvas.begin_frame();
        if self.editor.stroke.pending_len() > 0 {
            let dabs: Vec<_> = self.editor.stroke.drain_pending().collect();
            gfx.canvas.draw_dabs(&gfx.gpu.queue, &mut encoder, &dabs);
        }

        let viewport = Vec2::new(gfx.config.width as f32, gfx.config.height as f32);
        let layer_draws = self.editor.layer_draws();
        gfx.canvas.composite(
            &gfx.gpu.queue,
            &mut encoder,
            &view,
            &CompositeParams {
                camera: &self.editor.camera,
                viewport,
                layers: &layer_draws,
                active_index: self.editor.layers.active_index() as u32,
                stroke: self.editor.stroke_style,
            },
        );

        // --- egui on top ---
        let paint_jobs = gfx.egui_ctx.tessellate(shapes, pixels_per_point);
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [gfx.config.width, gfx.config.height],
            pixels_per_point,
        };
        for (id, delta) in &textures_delta.set {
            gfx.egui_renderer
                .update_texture(&gfx.gpu.device, &gfx.gpu.queue, *id, delta);
        }
        gfx.egui_renderer.update_buffers(
            &gfx.gpu.device,
            &gfx.gpu.queue,
            &mut encoder,
            &paint_jobs,
            &screen_descriptor,
        );
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
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
            gfx.egui_renderer
                .render(&mut pass.forget_lifetime(), &paint_jobs, &screen_descriptor);
        }
        for id in &textures_delta.free {
            gfx.egui_renderer.free_texture(id);
        }

        gfx.gpu.queue.submit(Some(encoder.finish()));
        surface_texture.present();

        // Keep the frames coming while a stroke is live; otherwise the app
        // goes back to sleep until the next input event.
        if self.editor.interaction == Interaction::Drawing {
            gfx.window.request_redraw();
        }

        // Applied after the `gfx` borrow ends, since these take `&mut self`.
        if actions.undo {
            self.undo();
        }
        if actions.redo {
            self.redo();
        }
        if actions.clear {
            self.clear_active_layer();
        }
        if actions.add_layer {
            self.add_layer();
        }
        if let Some(index) = actions.delete_layer {
            self.delete_layer(index);
        }
        if let Some(index) = actions.move_layer_up {
            self.editor.layers.move_up(index);
        }
        if let Some(index) = actions.move_layer_down {
            self.editor.layers.move_down(index);
        }
        if actions.fit_view {
            self.editor.camera = Camera::fit(self.editor.doc.size_vec2(), viewport);
        }
        if actions.reset_zoom {
            self.editor.camera.zoom = 1.0;
        }
    }
}

impl ApplicationHandler for UmberApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }

        let attrs = Window::default_attributes()
            .with_title("Umber")
            .with_inner_size(winit::dpi::LogicalSize::new(1440.0, 900.0));
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("failed to create window"),
        );

        let instance = Gpu::create_instance();
        let surface = instance
            .create_surface(window.clone())
            .expect("failed to create surface");
        let gpu = pollster::block_on(Gpu::new(instance, Some(&surface)))
            .expect("failed to initialise GPU");

        let size = window.inner_size();
        let config = gpu.surface_config(&surface, size.width, size.height);
        surface.configure(&gpu.device, &config);

        let canvas = CanvasRenderer::new(
            &gpu.device,
            UVec2::new(self.editor.doc.size.x, self.editor.doc.size.y),
            config.format,
        );

        // Start blank rather than showing whatever the allocation held.
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("init"),
            });
        canvas.clear_all_layers(&mut enc);
        canvas.clear_stroke(&mut enc);
        gpu.queue.submit(Some(enc.finish()));

        let egui_ctx = egui::Context::default();
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

        self.editor.camera = Camera::fit(
            self.editor.doc.size_vec2(),
            Vec2::new(config.width as f32, config.height as f32),
        );

        self.gfx = Some(Graphics {
            window,
            surface,
            config,
            gpu,
            canvas,
            egui_ctx,
            egui_state,
            egui_renderer,
        });
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        // Drop the surface but keep editor state; Android tears the window
        // down when backgrounded.
        self.gfx = None;
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(gfx) = self.gfx.as_mut() else { return };

        let response = gfx.egui_state.on_window_event(&gfx.window, &event);
        if response.repaint {
            gfx.window.request_redraw();
        }
        let ui_has_pointer = response.consumed || gfx.egui_ctx.egui_wants_pointer_input();
        let viewport = Vec2::new(gfx.config.width as f32, gfx.config.height as f32);

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                if size.width > 0 && size.height > 0 {
                    gfx.config.width = size.width;
                    gfx.config.height = size.height;
                    gfx.surface.configure(&gfx.gpu.device, &gfx.config);
                    gfx.window.request_redraw();
                }
            }

            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),

            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key
                    && self.handle_keys(code, event.state.is_pressed())
                    && let Some(g) = self.gfx.as_ref()
                {
                    g.window.request_redraw();
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                let pos = Vec2::new(position.x as f32, position.y as f32);
                self.editor.last_cursor = self.editor.cursor;
                self.editor.cursor = pos;

                match self.editor.interaction {
                    Interaction::Drawing => {
                        let point = self.editor.sample(pos, viewport, None);
                        self.editor.stroke.extend(point);
                        if let Some(g) = self.gfx.as_ref() {
                            g.window.request_redraw();
                        }
                    }
                    Interaction::Panning => {
                        let delta = pos - self.editor.last_cursor;
                        self.editor.camera.pan_by_screen(delta);
                        if let Some(g) = self.gfx.as_ref() {
                            g.window.request_redraw();
                        }
                    }
                    Interaction::Idle => {}
                }
            }

            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = state == ElementState::Pressed;
                let pan_button = button == MouseButton::Middle
                    || (button == MouseButton::Left && self.editor.space_down);

                if pan_button {
                    self.editor.interaction = if pressed {
                        Interaction::Panning
                    } else {
                        Interaction::Idle
                    };
                } else if button == MouseButton::Left {
                    if pressed && !ui_has_pointer {
                        let pos = self.editor.cursor;
                        self.editor.last_cursor = pos;
                        let point = self.editor.sample(pos, viewport, None);
                        self.start_stroke(point);
                    } else if !pressed {
                        self.finish_stroke();
                    }
                }
                if let Some(g) = self.gfx.as_ref() {
                    g.window.request_redraw();
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                if ui_has_pointer {
                    return;
                }
                let steps = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 60.0,
                };
                let factor = 1.12f32.powf(steps);
                self.editor
                    .camera
                    .zoom_at(self.editor.cursor, factor, viewport);
                if let Some(g) = self.gfx.as_ref() {
                    g.window.request_redraw();
                }
            }

            WindowEvent::Touch(touch) => {
                let pos = Vec2::new(touch.location.x as f32, touch.location.y as f32);
                // winit reports Force in either normalised or calibrated form;
                // `normalized` flattens both to 0..=1.
                let reported = touch.force.map(|f| f.normalized() as f32);

                match touch.phase {
                    TouchPhase::Started => {
                        self.editor.touches.insert(touch.id, pos);
                        if self.editor.touches.len() == 1 && !ui_has_pointer {
                            self.editor.cursor = pos;
                            self.editor.last_cursor = pos;
                            let point = self.editor.sample(pos, viewport, reported);
                            self.start_stroke(point);
                            self.editor.drawing_touch = Some(touch.id);
                        } else {
                            // A second finger means the gesture was a pinch,
                            // not a stroke. Abandon the stroke in progress.
                            self.cancel_stroke();
                            self.editor.drawing_touch = None;
                            self.update_pinch();
                        }
                    }
                    TouchPhase::Moved => {
                        self.editor.touches.insert(touch.id, pos);
                        if self.editor.drawing_touch == Some(touch.id) {
                            self.editor.last_cursor = self.editor.cursor;
                            self.editor.cursor = pos;
                            let point = self.editor.sample(pos, viewport, reported);
                            self.editor.stroke.extend(point);
                        } else {
                            self.update_pinch();
                        }
                    }
                    TouchPhase::Ended | TouchPhase::Cancelled => {
                        self.editor.touches.remove(&touch.id);
                        if self.editor.drawing_touch == Some(touch.id) {
                            self.finish_stroke();
                            self.editor.drawing_touch = None;
                        }
                        self.editor.pinch = None;
                    }
                }
                if let Some(g) = self.gfx.as_ref() {
                    g.window.request_redraw();
                }
            }

            WindowEvent::RedrawRequested => self.render(),

            _ => {}
        }
    }
}
