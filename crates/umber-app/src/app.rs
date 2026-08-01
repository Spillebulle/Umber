//! Window lifecycle, input translation and the frame loop.

use crate::canvasdlg;
use crate::editor::{Editor, Interaction, Tool};
use crate::logo;
use crate::session::{DocId, DocumentState};
use crate::shortcuts::{self, Action};
use crate::splash::{self, Splash};
use crate::tabs::{self, Notice};
use crate::theme::{self, Accent, ThemeKind};
use crate::ui;
use glam::{UVec2, Vec2};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use umber_core::docformat::{self, SaveDocument, SaveLayer};
use umber_core::{
    Brush, Color, Dab, Document, Edit, EditKind, InputPoint, Jump, PixelPatch, PixelRect,
};
use umber_render::{CanvasRenderer, CompositeParams, DabStyle, Gpu, ProbeParams};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
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
    /// One renderer per open document, keyed by [`DocId`].
    ///
    /// A document's pixels live in its renderer's texture array, so switching
    /// tabs is a different key in this map — no reallocation, no re-upload and
    /// nothing copied. Keyed by id rather than by tab position because
    /// positions shift when a tab is closed and a texture array must not
    /// change owner underneath a document.
    canvases: HashMap<DocId, CanvasRenderer>,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
}

impl Graphics {
    /// Give a document its own layer storage, cleared and ready to paint on.
    ///
    /// Built from an existing renderer where there is one: pipelines and
    /// shaders are shared, so this is a few textures rather than three shader
    /// compilations on the frame the user opened a document.
    /// `slots` is the document's slot high-water mark, not its layer count —
    /// see [`umber_core::LayerStack::slot_capacity_needed`]. A renderer starts
    /// with room for a handful of slices, and a document that already has more
    /// layers than that would otherwise hand the commit and undo paths a slice
    /// index the texture array does not have.
    fn add_canvas(&mut self, id: DocId, doc: &Document, slots: u32) {
        let size = doc.size;
        let mut canvas = match self.canvases.values().next() {
            Some(existing) => existing.for_document(&self.gpu.device, size),
            None => CanvasRenderer::new(&self.gpu.device, size, self.config.format),
        };
        canvas.ensure_slots(&self.gpu.device, &self.gpu.queue, slots);
        // The background belongs to this document, not to whichever one the
        // pipelines were cloned out of.
        canvas.set_background(doc.background);

        // Fresh textures hold whatever the allocation contained.
        let mut enc = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("init-document"),
            });
        canvas.clear_all_layers(&mut enc);
        canvas.clear_stroke(&mut enc);
        self.gpu.queue.submit(Some(enc.finish()));

        self.canvases.insert(id, canvas);
    }
}

/// The only thing that ever arrives on the event loop's user channel.
///
/// A background job has something to say. Under [`ControlFlow::Wait`] the loop
/// sleeps until an event arrives, and a value appearing in a channel is not an
/// event — so the thread sends one of these and the loop wakes to collect it.
/// It carries nothing: the value itself is in the channel, and the wake-up only
/// has to make a frame happen.
#[derive(Clone, Copy, Debug)]
pub struct Wake;

pub struct UmberApp {
    gfx: Option<Graphics>,
    editor: Editor,
    modifiers: ModifiersState,
    last_frame: Option<std::time::Instant>,
    /// Theme currently pushed into egui's style, so restyling only happens on
    /// an actual change.
    ///
    /// The accent is part of the key, not just the theme: it re-hues the
    /// palette, and egui's own tokens — selection fill, hyperlink colour —
    /// carry it too. Keyed on the theme alone, picking a new accent left those
    /// on the old hue until something else happened to trigger a restyle.
    applied_theme: Option<(ThemeKind, Accent)>,
    bindings: Vec<shortcuts::Binding>,
    /// When egui next wants to be redrawn, if it does — a fading hover, a
    /// blinking caret. `None` means "sleep until something happens".
    ///
    /// Kept here rather than acted on inside [`Self::render`] because setting
    /// the control flow needs the [`ActiveEventLoop`], which only the handler
    /// methods are given.
    repaint_at: Option<std::time::Instant>,
}

impl UmberApp {
    /// Build the application around an event loop it can wake.
    ///
    /// The proxy is handed straight to the update check, which is the only
    /// thing that ever answers from off the main thread. Everything else in
    /// Umber reaches the loop through a window event.
    pub fn new(proxy: EventLoopProxy<Wake>) -> Self {
        let mut editor = Editor::default();
        let updates_proxy = proxy.clone();
        editor.updates.set_waker(std::sync::Arc::new(move || {
            // The loop is gone once the window has closed, which is an ordinary
            // way for a check still in flight to end. Nothing to do about it.
            let _ = updates_proxy.send_event(Wake);
        }));
        // The autosave answers off the main thread too, and for the same reason
        // needs a way to say so: a tab's dot coming off is a frame nothing else
        // would ask for.
        editor.autosave.set_waker(std::sync::Arc::new(move || {
            let _ = proxy.send_event(Wake);
        }));
        Self {
            gfx: None,
            editor,
            modifiers: ModifiersState::default(),
            last_frame: None,
            applied_theme: None,
            bindings: shortcuts::defaults(),
            repaint_at: None,
        }
    }

    /// Finish the current stroke: capture undo state, bake it into the layer.
    ///
    /// The layer is untouched until this point, so reading it here captures
    /// exactly the pre-stroke pixels the undo stack needs.
    fn finish_stroke(&mut self) {
        let id = self.editor.session.active_id();
        let Some(gfx) = self.gfx.as_mut() else { return };
        let Some(canvas) = gfx.canvases.get_mut(&id) else {
            return;
        };
        if !self.editor.stroke.is_active() {
            return;
        }

        let bounds = self.editor.stroke.bounds();
        let dab_style = DabStyle {
            per_dab_color: self.editor.stroke.is_coloured(),
            build_up: self.editor.stroke.builds_up(),
        };
        self.editor.stroke.end();
        self.editor.interaction = Interaction::Idle;

        // Any smudge sample still in flight belongs to the stroke that is
        // ending. Letting one arrive during the next stroke would smear it with
        // a colour picked up somewhere else entirely.
        canvas.reset_probes();

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
            canvas.begin_frame();
            canvas.draw_dabs(&gfx.gpu.device, &gfx.gpu.queue, &mut enc, &tail, dab_style);
        }

        let Some(rect) = bounds.to_pixels_clamped(self.editor.doc.size) else {
            // Stroke fell entirely outside the canvas — nothing to commit, but
            // the scratch surface may still hold dabs.
            canvas.clear_stroke(&mut enc);
            gfx.gpu.queue.submit(Some(enc.finish()));
            return;
        };

        let slot = self.editor.stroke_slot;

        // Capture undo state first. `read_layer_rect` submits and blocks on its
        // own encoder, so it observes the layer before `enc` commits anything.
        let before = canvas.read_layer_rect(&gfx.gpu.device, &gfx.gpu.queue, slot, rect);
        // Labelled from the *snapshotted* style rather than from the brush in
        // hand, for the same reason the commit is: switching tool mid-stroke
        // must not change what the stroke that is ending turns out to have
        // been, in the history list any more than on the canvas.
        self.editor.history.record(Edit::new(
            EditKind::for_mode(self.editor.stroke_style.mode),
            PixelPatch::new(rect, slot, before),
        ));

        canvas.commit_stroke(
            &gfx.gpu.queue,
            &mut enc,
            slot,
            rect,
            self.editor.stroke_style,
        );
        gfx.gpu.queue.submit(Some(enc.finish()));
        // Something is now on the canvas that closing the tab would lose.
        self.editor.mark_modified();
    }

    /// Throw the in-progress stroke away without touching the layer.
    ///
    /// Used when a gesture turns out not to be a stroke — a second finger
    /// landing means the user meant to pinch, and the stray dab from the first
    /// finger should never reach the canvas or the undo stack.
    fn cancel_stroke(&mut self) {
        let id = self.editor.session.active_id();
        let Some(gfx) = self.gfx.as_mut() else { return };
        let Some(canvas) = gfx.canvases.get_mut(&id) else {
            return;
        };
        if !self.editor.stroke.is_active() {
            return;
        }
        self.editor.stroke.end();
        // Unlike a normal finish, these are dropped rather than flushed — the
        // whole point is that nothing from this gesture reaches the canvas.
        self.editor.stroke.clear_pending();
        canvas.reset_probes();
        self.editor.interaction = Interaction::Idle;

        let mut enc = gfx
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("cancel-stroke"),
            });
        canvas.clear_stroke(&mut enc);
        gfx.gpu.queue.submit(Some(enc.finish()));
    }

    /// Begin a stroke, guaranteeing the scratch surface starts empty and the
    /// right brush tip is bound.
    ///
    /// Every path that ends a stroke already clears the scratch, so that half is
    /// belt-and-braces — but stale coverage leaking into a new stroke is
    /// precisely the failure this module has already been bitten by, and it
    /// presents as a mystery colour change rather than as anything obvious.
    ///
    /// The tip is bound here and nowhere else. The dab pass binds one tip for
    /// the whole pass, so a thousand tipped dabs are still one draw call;
    /// changing it mid-stroke would restamp what is already in the scratch under
    /// a new shape. Between strokes is the only safe moment, and this is it.
    fn start_stroke(&mut self, point: InputPoint) {
        self.editor.begin_stroke(point);

        let id = self.editor.session.active_id();
        let tip = self.editor.tip.clone();
        if let Some(gfx) = self.gfx.as_mut()
            && let Some(canvas) = gfx.canvases.get_mut(&id)
        {
            // Cheap when the brush has not changed: `set_tip` compares the mask
            // by identity and returns without touching the GPU.
            canvas.set_tip(&gfx.gpu.device, &gfx.gpu.queue, tip);

            // The paper, on the same footing and for the same reasons: one
            // binding per pass, changed only between strokes. Read off the
            // *snapshotted* brush, so changing the Texture sliders mid-stroke
            // cannot re-texture the half already painted.
            let grain = self.editor.stroke.grain().and_then(|(strength, scale)| {
                let key = self.editor.brush.grain_pattern.key();
                umber_core::tip::pattern(key).map(|tile| (tile.clone(), strength, scale))
            });
            canvas.set_grain(&gfx.gpu.device, &gfx.gpu.queue, grain);

            let mut enc = gfx
                .gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("begin-stroke"),
                });
            canvas.clear_stroke(&mut enc);
            gfx.gpu.queue.submit(Some(enc.finish()));
        }
    }

    fn undo(&mut self) {
        let id = self.editor.session.active_id();
        let Some(gfx) = self.gfx.as_mut() else { return };
        let Some(canvas) = gfx.canvases.get(&id) else {
            return;
        };
        // The history is the live document's own, so this can only ever undo
        // work done on the canvas the user is looking at.
        let Some(edit) = self.editor.history.take_undo() else {
            return;
        };
        let patch = edit.patch;
        let current =
            canvas.read_layer_rect(&gfx.gpu.device, &gfx.gpu.queue, patch.slot, patch.rect);
        canvas.write_layer_rect(&gfx.gpu.queue, patch.slot, patch.rect, &patch.bytes);
        // The label travels with the entry rather than being recomputed, so an
        // undone stroke keeps its name on the far side of the cursor and the
        // list does not renumber itself as it is stepped through.
        self.editor.history.push_redo(Edit::new(
            edit.kind,
            PixelPatch::new(patch.rect, patch.slot, current),
        ));
        self.editor.mark_modified();
    }

    fn redo(&mut self) {
        let id = self.editor.session.active_id();
        let Some(gfx) = self.gfx.as_mut() else { return };
        let Some(canvas) = gfx.canvases.get(&id) else {
            return;
        };
        let Some(edit) = self.editor.history.take_redo() else {
            return;
        };
        let patch = edit.patch;
        let current =
            canvas.read_layer_rect(&gfx.gpu.device, &gfx.gpu.queue, patch.slot, patch.rect);
        canvas.write_layer_rect(&gfx.gpu.queue, patch.slot, patch.rect, &patch.bytes);
        self.editor.history.push_undo(Edit::new(
            edit.kind,
            PixelPatch::new(patch.rect, patch.slot, current),
        ));
        self.editor.mark_modified();
    }

    /// Move the document to `position` in the history — the number of recorded
    /// edits that should be applied — which is what clicking a row of the
    /// History module asks for.
    ///
    /// Carried out as that many single steps rather than as one jump. Each is a
    /// blocking read and write of one damaged rect, so a jump of eight costs
    /// exactly what eight presses of undo cost; a "jump" that restored a
    /// snapshot instead would need the snapshots this history exists not to
    /// keep. Acceptable on an explicit click and nowhere near the drawing loop.
    ///
    /// [`umber_core::History::steps_to`] clamps the count to what is held, so
    /// a click on a list drawn a frame ago cannot run past the end.
    fn jump_history(&mut self, position: usize) {
        match self.editor.history.steps_to(position) {
            Jump::Stay => {}
            Jump::Undo(steps) => {
                for _ in 0..steps {
                    self.undo();
                }
            }
            Jump::Redo(steps) => {
                for _ in 0..steps {
                    self.redo();
                }
            }
        }
    }

    /// Erase the active layer, leaving the rest of the stack alone.
    fn clear_active_layer(&mut self) {
        let slot = self.editor.layers.active_slot();
        let id = self.editor.session.active_id();
        let Some(gfx) = self.gfx.as_mut() else { return };
        let Some(canvas) = gfx.canvases.get(&id) else {
            return;
        };
        let mut enc = gfx
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("clear"),
            });
        canvas.clear_layer(&mut enc, slot);
        canvas.clear_stroke(&mut enc);
        gfx.gpu.queue.submit(Some(enc.finish()));
        // Undo entries reference pixels that no longer exist in any meaningful
        // sense; keeping them would let undo resurrect part of a cleared layer.
        self.editor.history.clear();
        self.editor.mark_modified();
    }

    fn add_layer(&mut self) {
        let Some(slot) = self.editor.layers.add() else {
            log::warn!("layer limit reached");
            return;
        };
        let needed = self.editor.layers.slot_capacity_needed();
        self.editor.mark_modified();

        let id = self.editor.session.active_id();
        let Some(gfx) = self.gfx.as_mut() else { return };
        let Some(canvas) = gfx.canvases.get_mut(&id) else {
            return;
        };
        canvas.ensure_slots(&gfx.gpu.device, &gfx.gpu.queue, needed);

        // A recycled slot still holds the deleted layer's pixels.
        let mut enc = gfx
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("init-layer"),
            });
        canvas.clear_layer(&mut enc, slot);
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
        self.editor.mark_modified();
    }

    fn handle_keys(&mut self, key: KeyCode, pressed: bool) -> bool {
        // Space is a held modifier with press *and* release meaning, which a
        // press-resolved binding table cannot express, so it stays separate.
        if key == KeyCode::Space {
            self.editor.space_down = pressed;
            return true;
        }
        if !pressed {
            return false;
        }

        let Some(action) = shortcuts::resolve(&self.bindings, key, self.modifiers) else {
            return false;
        };

        match action {
            Action::Save => {
                self.save_document(false);
            }
            Action::SaveAs => {
                self.save_document(true);
            }
            Action::Undo => self.undo(),
            Action::Redo => self.redo(),
            Action::BrushTool => self.editor.set_tool(Tool::Brush),
            Action::EraserTool => self.editor.set_tool(Tool::Eraser),
            Action::PanTool => self.editor.set_tool(Tool::Pan),
            Action::ZoomTool => self.editor.set_tool(Tool::Zoom),
            Action::SwapColours => self.editor.swap_colors(),
            Action::SizeDown => {
                self.editor.brush.size =
                    (self.editor.brush.size / 1.15).clamp(Brush::MIN_SIZE, Brush::MAX_SIZE);
            }
            Action::SizeUp => {
                self.editor.brush.size =
                    (self.editor.brush.size * 1.15).clamp(Brush::MIN_SIZE, Brush::MAX_SIZE);
            }
            Action::FitView => self.editor.fit_view(),
            Action::ActualSize => self.editor.camera.zoom = 1.0,
        }
        true
    }

    /// Take the colour under the cursor as the painting colour.
    fn pick_colour_at_cursor(&mut self) {
        let doc = self.editor.screen_to_doc(self.editor.cursor);
        let size = self.editor.doc.size;
        if doc.x < 0.0 || doc.y < 0.0 || doc.x >= size.x as f32 || doc.y >= size.y as f32 {
            return;
        }

        let layers = self.editor.layer_draws();
        let id = self.editor.session.active_id();
        let Some(gfx) = self.gfx.as_ref() else { return };
        let Some(canvas) = gfx.canvases.get(&id) else {
            return;
        };
        let px = canvas.pick_colour(&gfx.gpu.device, &gfx.gpu.queue, &layers, doc);

        // Transparent means there is nothing to pick; taking it would silently
        // set the brush to black.
        if px[3] == 0 {
            return;
        }
        self.editor
            .set_color(Color::from_srgb_u8(px[0], px[1], px[2], 255));
    }

    /// Write the whole layered document out, and remember where it went.
    ///
    /// Returns whether a file was actually written, which is what lets the
    /// close prompt treat a cancelled dialog as "not yet" rather than as
    /// permission to discard the document.
    ///
    /// `always_ask` is Save as…: the file is chosen even when the document
    /// already has one.
    /// Drop any autosave capture reading `id`, because what it is reading is
    /// about to stop being what the document holds.
    ///
    /// Called wherever an autosave could otherwise finish *after* something
    /// that supersedes it: an explicit Save, a resize, a document closing. The
    /// renderer half and the scheduler half both have to be told — the first
    /// gives the staging buffer back, the second stops waiting for pixels that
    /// are not coming.
    fn stop_autosave_of(&mut self, id: DocId) {
        if self.editor.autosave.capturing_id() != Some(id) {
            return;
        }
        if let Some(gfx) = self.gfx.as_mut()
            && let Some(canvas) = gfx.canvases.get_mut(&id)
        {
            canvas.cancel_capture();
        }
        self.editor.autosave.abandon();
    }

    fn save_document(&mut self, always_ask: bool) -> bool {
        let id = self.editor.session.active_id();
        // An autosave already reading this document would land on the file
        // about to be written, a stroke or two behind it. The explicit save
        // wins: it is the one the painter asked for.
        self.stop_autosave_of(id);
        let existing = self.editor.session.active_tab().path.clone();
        let path = match existing {
            Some(path) if !always_ask => path,
            existing => {
                let suggested = match &existing {
                    Some(path) => path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    None => format!(
                        "{}.{}",
                        self.editor.session.active_title(),
                        docformat::EXTENSION
                    ),
                };
                let Some(picked) = rfd::FileDialog::new()
                    .set_title(if always_ask {
                        "Save document as"
                    } else {
                        "Save document"
                    })
                    .add_filter("OpenRaster document", &[docformat::EXTENSION])
                    .set_file_name(suggested)
                    .save_file()
                else {
                    return false;
                };
                with_extension(picked)
            }
        };

        let name = file_name_of(&path);
        let size = self.editor.doc.size;
        let rect = PixelRect {
            x: 0,
            y: 0,
            width: size.x,
            height: size.y,
        };

        // Scoped so every borrow of the editor and the GPU is released before
        // the outcome is acted on, which needs `&mut self`.
        let outcome = {
            let Some(gfx) = self.gfx.as_ref() else {
                return false;
            };
            let Some(canvas) = gfx.canvases.get(&id) else {
                return false;
            };
            let stack = self.editor.layers.layers();

            // Every layer comes off the GPU whole, and all of them are held at
            // once — 16 MB each at 2048², so a full stack is a few hundred
            // megabytes for as long as the save takes. That is the price of a
            // format that keeps layers, and `read_layer_rect` blocks, which is
            // why this is only ever reached from an explicit Save and never
            // from the drawing loop.
            let pixels: Vec<Vec<u8>> = stack
                .iter()
                .map(|layer| {
                    canvas.read_layer_rect(&gfx.gpu.device, &gfx.gpu.queue, layer.slot(), rect)
                })
                .collect();
            // The flattened preview the format requires comes from the same
            // composite pass the screen uses, so it cannot disagree with it.
            let merged =
                canvas.export_rgba(&gfx.gpu.device, &gfx.gpu.queue, &self.editor.layer_draws());

            let layers: Vec<SaveLayer<'_>> = stack
                .iter()
                .zip(&pixels)
                .map(|(layer, px)| SaveLayer {
                    name: &layer.name,
                    visible: layer.visible,
                    opacity: layer.opacity,
                    blend: layer.blend,
                    pixels: px,
                })
                .collect();

            // The undo history, resolved against the stack it belongs to. No
            // GPU work: the patches have been in memory since they were
            // captured at commit time, so this adds nothing to the blocking
            // readbacks above. `SaveHistory::new` refuses outright if any patch
            // names a slot no layer holds, which cannot happen from here —
            // deleting a layer clears the history — and is checked anyway
            // because a patch replayed into the wrong layer is far worse than
            // no saved history at all.
            let history = self
                .editor
                .ui
                .save_history
                .then(|| docformat::SaveHistory::new(&self.editor.history, &self.editor.layers))
                .flatten();

            docformat::save(
                &path,
                &SaveDocument {
                    size,
                    layers: &layers,
                    active: self.editor.layers.active_index(),
                    background: self.editor.doc.background,
                    dpi: self.editor.doc.dpi,
                    merged: &merged,
                    history,
                },
            )
        };

        match outcome {
            Ok(warnings) => {
                log::info!(
                    "saved {} — {} × {}, {} layer(s)",
                    path.display(),
                    size.x,
                    size.y,
                    self.editor.layers.len(),
                );
                self.editor.mark_saved(path);
                // The document has just been written, so its autosave clock
                // starts again from here. Without this the very next brush
                // stroke would trigger a full autosave of a document saved a
                // second ago.
                self.editor.autosave.defer(id, std::time::Instant::now());
                // Anything the format could not carry exactly is said out loud,
                // for the same reason an import says what it dropped.
                if !warnings.is_empty() {
                    self.editor.notice = Some(Notice {
                        title: format!("“{name}” saved with changes"),
                        lines: warnings.iter().map(ToString::to_string).collect(),
                    });
                }
                true
            }
            Err(error) => {
                log::error!("could not save {}: {error}", path.display());
                self.editor.notice = Some(Notice {
                    title: format!("Could not save “{name}”"),
                    lines: vec![error.to_string()],
                });
                false
            }
        }
    }

    /// Flatten the visible stack and write it to a PNG the user picks.
    fn export_png(&mut self) {
        let id = self.editor.session.active_id();
        // The tab is named after its file once it has one, so the suggestion
        // has to lose that extension or it comes out as `sketch.ora.png`.
        let stem = self.editor.session.active_title();
        let stem = stem
            .strip_suffix(&format!(".{}", docformat::EXTENSION))
            .unwrap_or(stem);
        let suggested = format!("{stem}.png");
        let Some(gfx) = self.gfx.as_ref() else { return };
        let Some(canvas) = gfx.canvases.get(&id) else {
            return;
        };

        let Some(path) = rfd::FileDialog::new()
            .set_title("Export PNG")
            .add_filter("PNG image", &["png"])
            .set_file_name(suggested)
            .save_file()
        else {
            return;
        };

        let layers = self.editor.layer_draws();
        let pixels = canvas.export_rgba(&gfx.gpu.device, &gfx.gpu.queue, &layers);

        let size = self.editor.doc.size;
        match write_png(&path, size.x, size.y, &pixels) {
            Ok(()) => log::info!("exported {}", path.display()),
            Err(e) => log::error!("could not write {}: {e}", path.display()),
        }
    }

    /// Two-finger pinch: pan by the midpoint delta, zoom by the spread ratio.
    fn update_pinch(&mut self) {
        // The pivot, not the viewport size. `zoom_at` keeps the document point
        // under the anchor pinned, and it can only do that against the same
        // pivot the composite pass is given — the centre of the canvas region.
        // Handing it the window size instead made a pinch drag the canvas away
        // from the fingers doing it.
        let pivot = self.editor.canvas_pivot;
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
                self.editor.camera.zoom_at(mid, factor, pivot);
            }
        }
        self.editor.pinch = Some((dist, mid));
    }

    fn render(&mut self) {
        // Before the interface is built, so an answer that arrived while the
        // loop was asleep is on screen in the frame the wake-up produced rather
        // than in the one after it.
        self.editor.updates.poll();

        let Some(gfx) = self.gfx.as_mut() else { return };

        let now = std::time::Instant::now();
        if let Some(prev) = self.last_frame {
            self.editor.record_frame_time((now - prev).as_secs_f32());
        }
        self.last_frame = Some(now);

        // Restyling walks the whole style struct, so only do it when the theme
        // actually changed rather than every frame.
        let wanted = (self.editor.ui.theme, self.editor.ui.accent);
        if self.applied_theme != Some(wanted) {
            theme::apply(
                &gfx.egui_ctx,
                &theme::Palette::with_accent(wanted.0, wanted.1),
            );
            self.applied_theme = Some(wanted);
        }

        // --- UI ---
        // `Context::run_ui` discards the closure's return value, so the panel's
        // output is captured out of it instead.
        let editor = &mut self.editor;
        let mut actions = ui::UiActions::default();
        let mut canvas_rect = egui::Rect::ZERO;
        let raw_input = gfx.egui_state.take_egui_input(&gfx.window);
        let full_output = gfx.egui_ctx.run_ui(raw_input, |ui| {
            let out = ui::draw(ui, editor);
            actions = out.actions;
            canvas_rect = out.canvas_rect;
        });

        let egui::FullOutput {
            platform_output,
            textures_delta,
            shapes,
            pixels_per_point,
            viewport_output,
        } = full_output;
        gfx.egui_state
            .handle_platform_output(&gfx.window, platform_output);

        // What egui itself wants next, which is the *only* thing that should
        // schedule a frame with no input behind it.
        //
        // This used to be missing, and `window_event` used to request a redraw
        // whenever egui-winit set `repaint`. egui-winit sets it for
        // `RedrawRequested` — so painting a frame asked for another one, for
        // ever. `ControlFlow::Wait` never got to wait: the app burned a fifth
        // of a core sitting still, vsync being the only thing holding it back.
        self.repaint_at = viewport_output
            .get(&gfx.egui_ctx.viewport_id())
            .and_then(|out| {
                if out.repaint_delay.is_zero() {
                    gfx.window.request_redraw();
                    None
                } else {
                    // `Duration::MAX` is egui's "nothing pending"; anything
                    // shorter is an animation asking to be continued.
                    std::time::Instant::now().checked_add(out.repaint_delay)
                }
            });

        // egui works in points; the canvas works in physical pixels.
        self.editor.pixels_per_point = pixels_per_point;
        self.editor.canvas_pivot = Vec2::new(
            canvas_rect.center().x * pixels_per_point,
            canvas_rect.center().y * pixels_per_point,
        );
        self.editor.canvas_size = Vec2::new(
            canvas_rect.width() * pixels_per_point,
            canvas_rect.height() * pixels_per_point,
        );

        // Tessellation and egui's texture uploads happen *before* the surface
        // is acquired, because every path below this can decide not to draw the
        // frame — a resize, a minimise, a lost swapchain.
        //
        // A skipped frame used to drop `textures_delta` on the floor. egui
        // sends a whole image when the font atlas is created and only the new
        // region as glyphs are added to it, so losing the first delta leaves
        // egui-wgpu with a partial update for a texture it never allocated —
        // which it meets with `.expect("Tried to update a texture that has not
        // been allocated yet.")`. Resizing the window while a new glyph is
        // rasterised was enough.
        let paint_jobs = gfx.egui_ctx.tessellate(shapes, pixels_per_point);
        for (id, delta) in &textures_delta.set {
            gfx.egui_renderer
                .update_texture(&gfx.gpu.device, &gfx.gpu.queue, *id, delta);
        }

        let acquired = match gfx.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => Some(t),
            // Suboptimal still gives a usable texture; reconfiguring is a
            // next-frame concern, so draw this one rather than dropping it.
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => {
                gfx.surface.configure(&gfx.gpu.device, &gfx.config);
                Some(t)
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                gfx.surface.configure(&gfx.gpu.device, &gfx.config);
                None
            }
            // Minimised or hidden — skip the frame entirely.
            wgpu::CurrentSurfaceTexture::Occluded | wgpu::CurrentSurfaceTexture::Timeout => None,
            other => {
                log::warn!("could not acquire surface texture: {other:?}");
                None
            }
        };

        // The renderer of the document in front. Every other open document has
        // one of its own, holding its pixels, untouched until it is switched to.
        let has_canvas = gfx.canvases.contains_key(&self.editor.session.active_id());
        if !has_canvas {
            log::error!("the active document has no canvas renderer");
        }

        let Some(surface_texture) = acquired.filter(|_| has_canvas) else {
            // Frees are applied even though nothing was drawn: they name
            // textures egui has finished with, which the jobs just discarded
            // above cannot reference.
            for id in &textures_delta.free {
                gfx.egui_renderer.free_texture(id);
            }
            return;
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

        // --- autosave ---
        // Into this frame's own encoder, so a document being read back off the
        // GPU costs one recorded copy rather than a submission of its own. It
        // may be reading a document that is not the one in front: every open
        // document is autosaved, in turn, and a background document's pixels
        // are in its own renderer.
        //
        // `quiet` is what keeps this out of a stroke. Nothing starts while the
        // pointer is doing anything at all — so "every five minutes" is really
        // "at the first quiet moment after five minutes", which is also what a
        // painter would choose.
        let quiet = self.editor.interaction == Interaction::Idle
            && !self.editor.stroke.is_active()
            && self.editor.touches.is_empty();
        crate::autosave::drive(
            &mut self.editor,
            &gfx.gpu,
            &mut gfx.canvases,
            &mut encoder,
            quiet,
        );

        // --- canvas ---
        // Presence was established above, before the surface was acquired, so
        // that a missing renderer is not a frame abandoned with a swapchain
        // image in hand.
        let Some(canvas) = gfx.canvases.get_mut(&self.editor.session.active_id()) else {
            return;
        };
        canvas.begin_frame();
        let dab_style = DabStyle {
            per_dab_color: self.editor.stroke.is_coloured(),
            build_up: self.editor.stroke.builds_up(),
        };
        if self.editor.stroke.pending_len() > 0 {
            let dabs: Vec<_> = self.editor.stroke.drain_pending().collect();
            canvas.draw_dabs(
                &gfx.gpu.device,
                &gfx.gpu.queue,
                &mut encoder,
                &dabs,
                dab_style,
            );
        }

        let layer_draws = self.editor.layer_draws();

        // A smudging brush needs to know what it is passing over. The read is
        // asynchronous: this records a sample and collects whichever earlier one
        // has come home, so no frame ever waits on the GPU. The probe is taken
        // *after* this frame's dabs so a brush scrubbed back and forth picks up
        // its own wet paint, and before the screen composite so the two share
        // the encoder.
        if let Some((point, radius)) = self.editor.stroke.probe() {
            canvas.probe_canvas(
                &gfx.gpu.device,
                &gfx.gpu.queue,
                &mut encoder,
                &ProbeParams {
                    layers: &layer_draws,
                    active_index: self.editor.layers.active_index() as u32,
                    stroke: self.editor.stroke_style,
                    doc_point: point,
                    radius,
                },
            );
        }
        canvas.composite(
            &gfx.gpu.queue,
            &mut encoder,
            &view,
            &CompositeParams {
                camera: &self.editor.camera,
                pivot: self.editor.canvas_pivot,
                layers: &layer_draws,
                active_index: self.editor.layers.active_index() as u32,
                stroke: self.editor.stroke_style,
                backdrop: theme::Palette::with_accent(self.editor.ui.theme, self.editor.ui.accent)
                    .backdrop_display(),
                export: false,
            },
        );

        // --- egui on top ---
        // Tessellated and uploaded above, before the surface was acquired.
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [gfx.config.width, gfx.config.height],
            pixels_per_point,
        };
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

        // The probe's copy is only submitted now, so mapping it has to wait
        // until here. Collecting is a non-blocking poll: whatever came home
        // feeds the stroke, and whatever did not is picked up next frame.
        if dab_style.per_dab_color
            && let Some(canvas) = gfx.canvases.get_mut(&self.editor.session.active_id())
        {
            canvas.submit_probes();
            if let Some(sample) = canvas.take_probe(&gfx.gpu.device) {
                self.editor.stroke.absorb(sample);
            }
        }

        // The autosave's own readback, mapped now that the frame holding its
        // copy has been submitted, and collected by a poll that never waits.
        // Anything the writer thread has finished is applied here too.
        if let Some(notice) =
            crate::autosave::collect(&mut self.editor, &gfx.gpu, &mut gfx.canvases)
        {
            self.editor.notice = Some(notice);
        }

        // Keep the frames coming while a stroke is live; otherwise the app
        // goes back to sleep until the next input event. A capture in flight
        // needs the same: under `ControlFlow::Wait` a document being read back
        // would otherwise stop dead the moment the painter took their hand off
        // the mouse, which is exactly when it started.
        if self.editor.interaction == Interaction::Drawing || self.editor.autosave.capturing() {
            gfx.window.request_redraw();
        }

        // Applied after the `gfx` borrow ends, since these take `&mut self`.
        if actions.undo {
            self.undo();
        }
        if actions.redo {
            self.redo();
        }
        if let Some(position) = actions.history_jump {
            self.jump_history(position);
        }
        if actions.clear {
            self.clear_active_layer();
        }
        if actions.export {
            self.export_png();
        }
        if actions.save {
            self.save_document(false);
        }
        if actions.save_as {
            self.save_document(true);
        }
        if let Some(index) = actions.save_and_close {
            // The prompt is always raised on the document in front, but the
            // pairing is made explicit here: saving one tab and closing another
            // would lose work in the most confusing way available. A switch to
            // the tab that is already active costs nothing.
            self.switch_document(index);
            // Closed only on a written file. A cancelled dialog or a failed
            // write leaves the tab open, still holding what it was about to
            // lose.
            if self.save_document(false) {
                self.close_document(index);
            }
        }
        if actions.add_layer {
            self.add_layer();
        }
        if let Some(index) = actions.delete_layer {
            self.delete_layer(index);
        }
        if let Some(index) = actions.move_layer_up {
            self.editor.layers.move_up(index);
            self.editor.mark_modified();
        }
        if let Some(index) = actions.move_layer_down {
            self.editor.layers.move_down(index);
            self.editor.mark_modified();
        }
        if actions.fit_view {
            self.editor.fit_view();
        }
        if actions.reset_zoom {
            self.editor.camera.zoom = 1.0;
        }
        if let Some(index) = actions.pick_tab {
            self.switch_document(index);
        }
        if actions.new_document {
            self.new_document();
        }
        if let Some(doc) = actions.create_document {
            self.create_document(doc);
        }
        if let Some(change) = actions.canvas_change {
            self.apply_canvas(change);
        }
        if actions.open_file {
            self.open_file();
        }
        if let Some(index) = actions.close_tab {
            self.close_document(index);
        }
        if actions.reveal_autosaves
            && let Some(dir) = crate::autosave::internal_dir()
            && let Err(e) = crate::autosave::reveal(&dir)
        {
            // Logged rather than shown: the settings dialog prints the path
            // beside the button, so somebody whose desktop has no file manager
            // still has what they need.
            log::warn!("could not open {}: {e}", dir.display());
        }

        // Last, and after the interface has run at least once: the preferences
        // file is read by `settings::show`, so this is the first point at which
        // "does the user want this check?" has an answer. Returns immediately
        // on every frame but one.
        self.editor.updates.start_if_due();
    }

    // --- documents -------------------------------------------------------

    /// Show another open document.
    ///
    /// The state swap is the editor's; all that happens here is that a stroke
    /// still in flight is finished into the document it was started on, before
    /// that document stops being the one the renderer writes to.
    fn switch_document(&mut self, index: usize) {
        if index == self.editor.session.active_index() {
            return;
        }
        self.finish_stroke();
        if self.editor.switch_tab(index) {
            self.request_redraw();
        }
    }

    fn new_document(&mut self) {
        let doc = self.editor.doc;
        self.create_document(doc);
    }

    /// Open a blank document with the settings the New dialog was given.
    fn create_document(&mut self, doc: Document) {
        self.finish_stroke();
        let id = self.editor.create_document(doc);
        let slots = self.editor.layers.slot_capacity_needed();
        if let Some(gfx) = self.gfx.as_mut() {
            gfx.add_canvas(id, &doc, slots);
        }
        self.request_redraw();
    }

    /// Apply the Canvas settings dialog's answer to the document in front.
    ///
    /// The stroke is finished first: a resize throws the scratch surface away,
    /// so a stroke still in flight would be lost rather than committed. Then
    /// the editor takes the new document — which is also what clears the undo
    /// history when the geometry moves — and the GPU carries the pixels across.
    fn apply_canvas(&mut self, change: canvasdlg::CanvasChange) {
        self.finish_stroke();
        let id = self.editor.session.active_id();
        // A resize throws the layer textures away and rebuilds them, so a
        // capture part-way through would assemble a file out of layers of two
        // different sizes. `CanvasRenderer::resize` cancels its own half; this
        // is the scheduler's.
        self.stop_autosave_of(id);
        let doc = change.doc;
        let resized = self.editor.apply_canvas(doc);

        if let Some(gfx) = self.gfx.as_mut()
            && let Some(canvas) = gfx.canvases.get_mut(&id)
        {
            if resized {
                canvas.resize(&gfx.gpu.device, &gfx.gpu.queue, doc.size, change.anchor);
            }
            canvas.set_background(doc.background);
        }
        self.request_redraw();
    }

    /// Close a document and free the GPU storage that was holding its pixels.
    fn close_document(&mut self, index: usize) {
        self.finish_stroke();
        self.editor.ui.close_prompt = None;
        // Before the tab goes: the renderer is about to be dropped, and the
        // scheduler would otherwise wait for ever for a document that no longer
        // exists — which would stop every *other* document being autosaved,
        // since only one capture runs at a time.
        if let Some(id) = self.editor.session.tabs().get(index).map(|t| t.id) {
            self.stop_autosave_of(id);
        }
        let Some(closed) = self.editor.close_tab(index) else {
            return;
        };
        if let Some(gfx) = self.gfx.as_mut() {
            // Dropping the renderer releases the layer texture array — several
            // megabytes per document, which is the whole reason closing a tab
            // has to reach the GPU at all.
            gfx.canvases.remove(&closed);
        }
        self.request_redraw();
    }

    /// Open a document written by another application.
    fn open_file(&mut self) {
        let extensions = umber_core::docimport::supported_extensions();
        let Some(path) = rfd::FileDialog::new()
            .set_title("Open document")
            // Built from the importer's own list rather than typed out again,
            // so a format added there appears here without a second edit.
            .add_filter("Paintable documents", extensions)
            .add_filter("All files", &["*"])
            .pick_file()
        else {
            return;
        };
        self.open_path(&path);
    }

    fn open_path(&mut self, path: &Path) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());

        let imported = match umber_core::docimport::import(path) {
            Ok(doc) => doc,
            Err(error) => {
                // `ImportError` displays as a finished sentence written for the
                // user; showing it verbatim beats inventing a second wording.
                log::warn!("could not open {}: {error}", path.display());
                self.editor.notice = Some(Notice {
                    title: format!("Could not open “{name}”"),
                    lines: vec![error.to_string()],
                });
                return;
            }
        };

        // The importer bounds itself at 16384 px, but the device is the
        // authority — and it is asked before any of this becomes a document,
        // so a refusal leaves the session exactly as it was.
        if let Some(gfx) = self.gfx.as_ref() {
            let max = gfx.gpu.device.limits().max_texture_dimension_2d;
            if imported.size.x > max || imported.size.y > max {
                self.editor.notice = Some(Notice {
                    title: format!("Could not open “{name}”"),
                    lines: vec![format!(
                        "The canvas is {} × {}, and this GPU cannot hold a texture \
                         larger than {max} pixels on a side.",
                        imported.size.x, imported.size.y,
                    )],
                });
                return;
            }
        }

        let format = imported.format.label();
        let notes = tabs::summarise(&imported.warnings);
        let umber_core::docimport::Opened {
            document: doc,
            stack: layers,
            uploads,
            // Empty unless the file carried one that resolved against the stack
            // above, and the two are built together for that reason: the
            // patches name stack positions in the file and texture slots here,
            // and the slots do not exist until the stack does.
            history,
        } = imported.open();
        let size = doc.size;
        let slots = layers.slot_capacity_needed();

        let id = self.editor.open_document(
            DocumentState {
                camera: umber_core::Camera::fit(doc.size_vec2(), self.editor.canvas_size),
                doc,
                layers,
                history,
            },
            name.clone(),
            Some(path.to_path_buf()),
            notes.clone(),
        );

        if let Some(gfx) = self.gfx.as_mut() {
            gfx.add_canvas(id, &doc, slots);
            if let Some(canvas) = gfx.canvases.get_mut(&id) {
                for upload in &uploads {
                    canvas.write_layer_rect(
                        &gfx.gpu.queue,
                        upload.slot,
                        umber_core::PixelRect {
                            x: 0,
                            y: 0,
                            width: size.x,
                            height: size.y,
                        },
                        &upload.pixels,
                    );
                }
            }
        }

        log::info!(
            "opened {} as {format}, {} × {}, {} layer(s)",
            path.display(),
            size.x,
            size.y,
            uploads.len(),
        );

        // Losses are reported, never left in the log: a painter who opens a
        // Photoshop file and finds it flattened deserves to be told why.
        if !notes.is_empty() {
            self.editor.notice = Some(Notice {
                title: format!("“{name}” opened with changes"),
                lines: notes,
            });
        }
        self.request_redraw();
    }

    fn request_redraw(&self) {
        if let Some(gfx) = self.gfx.as_ref() {
            gfx.window.request_redraw();
        }
    }
}

impl ApplicationHandler<Wake> for UmberApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }

        // The window appears early and then sits unpainted until the rest of
        // this function has finished, so this is the number that decides
        // whether Umber needs a real loading screen. Logged rather than
        // measured once and written down, because it is a property of the
        // driver and the machine rather than of this code.
        let started = std::time::Instant::now();

        let attrs = Window::default_attributes()
            .with_title("Umber")
            // Per-platform, and quietly so: Windows uses it for the title bar
            // and taskbar button, X11 for the window list. Wayland ignores it
            // entirely and takes its icon from the `.desktop` file matching the
            // app id, and on macOS `set_window_icon` is documented as a no-op —
            // there the icon comes from the `.app` bundle's `Info.plist`. The
            // executable resource in `crates/umber-desktop/build.rs` is what
            // gives Explorer and the taskbar an icon before the process starts.
            .with_window_icon(logo::window_icon())
            .with_inner_size(winit::dpi::LogicalSize::new(1440.0, 900.0));
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("failed to create window"),
        );

        let window_ready = started.elapsed();

        // From here to the end of this function the window is on screen with
        // nothing in it, and most of the wait is the graphics driver's. The
        // splash paints it from the CPU — the only way to reach a window that
        // has no GPU surface yet. Each stage is shown *before* the work it
        // names, so the bar never claims progress that has not happened.
        let mut splash = Splash::new(
            window.clone(),
            theme::Palette::with_accent(self.editor.ui.theme, self.editor.ui.accent),
        );
        splash.show(splash::Stage::Adapter);

        let instance = Gpu::create_instance();
        let surface = instance
            .create_surface(window.clone())
            .expect("failed to create surface");
        let gpu = pollster::block_on(Gpu::new(instance, Some(&surface)))
            .expect("failed to initialise GPU");

        splash.adapter(&gpu.adapter.get_info());
        splash.show(splash::Stage::Surface);

        let size = window.inner_size();
        let config = gpu.surface_config(&surface, size.width, size.height);
        surface.configure(&gpu.device, &config);

        splash.show(splash::Stage::Shaders);
        // Compiled once here; every further document clones the pipeline
        // handles out of this one. See `Graphics::add_canvas`.
        let mut canvas = CanvasRenderer::new(
            &gpu.device,
            UVec2::new(self.editor.doc.size.x, self.editor.doc.size.y),
            config.format,
        );
        // At start-up this is one layer and does nothing. It matters on the
        // Android path, where `resumed` runs again with a session already open:
        // the live document can have any number of layers, and its slots have
        // to exist before the first stroke commits into one.
        canvas.ensure_slots(
            &gpu.device,
            &gpu.queue,
            self.editor.layers.slot_capacity_needed(),
        );
        // This one renderer is built here rather than by `add_canvas`, so it
        // needs the same call: a renderer starts on transparency until it is
        // told what its document is on, and a new document is white.
        canvas.set_background(self.editor.doc.background);

        // Start blank rather than showing whatever the allocation held.
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("init"),
            });
        canvas.clear_all_layers(&mut enc);
        canvas.clear_stroke(&mut enc);
        gpu.queue.submit(Some(enc.finish()));

        splash.show(splash::Stage::Fonts);
        let egui_ctx = egui::Context::default();
        theme::install_fonts(&egui_ctx);
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

        // Everything is ready, so the splash goes now — not when its bar
        // finishes. A progress bar that held the first frame back to complete
        // its own animation would be a lie about latency in an application that
        // exists for latency.
        splash.show(splash::Stage::Ready);
        drop(splash);

        // Provisional: the real canvas region is not known until the first
        // frame has laid the panels out, which corrects both of these.
        self.editor.canvas_size = Vec2::new(config.width as f32, config.height as f32);
        self.editor.canvas_pivot = self.editor.canvas_size * 0.5;
        self.editor.fit_view();

        let documents = self.editor.open_documents();
        let active = self.editor.session.active_id();
        let mut canvases = HashMap::new();
        canvases.insert(active, canvas);

        self.gfx = Some(Graphics {
            window,
            surface,
            config,
            gpu,
            canvases,
            egui_ctx,
            egui_state,
            egui_renderer,
        });

        // Normally there is exactly one document here, this being start-up.
        // The loop is for the Android path, where the surface is destroyed on
        // suspend and rebuilt on resume with the session still open: every
        // document needs its storage back. Their pixels do not survive that —
        // they never have — but a document without a renderer would be a blank
        // window with no way out.
        if documents.len() > 1 {
            log::warn!(
                "rebuilding storage for {} documents after the surface was lost; \
                 their contents are gone",
                documents.len(),
            );
        }
        if let Some(gfx) = self.gfx.as_mut() {
            for (id, doc, slots) in documents {
                if id != active {
                    gfx.add_canvas(id, &doc, slots);
                }
            }
        }

        // Ask for the first frame explicitly. The platform usually sends one
        // unprompted as the window is shown, but "usually" is not a thing to
        // rest a blank window on — and nothing else here would ever wake the
        // loop, now that painting a frame no longer asks for another.
        self.request_redraw();

        // Split, because the two halves have very different characters and only
        // one of them is ours to improve: window creation is fast and constant,
        // while adapter enumeration and device creation are the graphics
        // driver's own start-up and dominate the total. `splash.rs` explains
        // what these numbers mean for the overlay it draws.
        log::info!(
            "window in {:.0} ms, GPU and fonts {:.0} ms more",
            window_ready.as_secs_f64() * 1000.0,
            (started.elapsed() - window_ready).as_secs_f64() * 1000.0
        );
    }

    /// Decide how long to sleep for.
    ///
    /// [`ControlFlow::Wait`] unless egui asked for a frame at a particular
    /// time — a hover fading, a caret blinking. Without this the only thing
    /// keeping those animations moving would be a redraw loop that never idles,
    /// which is what this replaced.
    /// A background job has reported. [`Self::render`] is what collects it; all
    /// this has to do is make a frame happen.
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: Wake) {
        self.request_redraw();
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // The Windows installer cannot replace a program that is running, so
        // handing it the package is only half of the update — the other half is
        // getting out of its way, which the user asks for in the About dialog.
        if self.editor.updates.take_quit_request() {
            event_loop.exit();
            return;
        }
        match self.repaint_at {
            Some(at) => event_loop.set_control_flow(ControlFlow::WaitUntil(at)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: winit::event::StartCause) {
        // The deadline set above has come round: egui wants the frame it asked
        // for. Nothing else wakes the loop on a timer.
        if matches!(cause, winit::event::StartCause::ResumeTimeReached { .. }) {
            self.repaint_at = None;
            self.request_redraw();
        }
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        // Drop the surface but keep editor state; Android tears the window
        // down when backgrounded.
        self.gfx = None;
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(gfx) = self.gfx.as_mut() else { return };

        let response = gfx.egui_state.on_window_event(&gfx.window, &event);
        // egui-winit reports `repaint` for `RedrawRequested` itself, which is a
        // tautology: acting on it means every painted frame asks for the next
        // one, and `ControlFlow::Wait` never gets to wait. What egui genuinely
        // wants next is read from its `viewport_output` in `render`.
        if response.repaint && !matches!(event, WindowEvent::RedrawRequested) {
            gfx.window.request_redraw();
        }
        // Who owns the pointer, in three parts.
        //
        // This used to ask egui, via `response.consumed` and
        // `egui_wants_pointer_input()`. Both are built on
        // `Context::is_pointer_over_egui`, which since egui 0.35 answers *true
        // everywhere*: `CentralPanel` now consumes the root `Ui`'s cursor, so
        // the unused rect it tests against is empty by the end of the pass.
        // With it true, `egui_wants_pointer_input()` is true on every fresh
        // press — and the press that begins a stroke was being swallowed.
        //
        // So decide it here instead:
        //
        // * `egui_is_using_pointer` — a slider or scrollbar has the drag. This
        //   is the one part of egui's answer that does not depend on the broken
        //   test.
        // * a non-background layer under the cursor — a menu, a popup, or a
        //   floating panel, all of which are `Area`s and all of which sit over
        //   the canvas rather than beside it.
        // * `pointer_over_canvas` — the canvas region itself, minus whatever
        //   the layout has claimed, computed from the same rect the composite
        //   pass is given.
        let over_area = gfx
            .egui_ctx
            .layer_id_at(self.editor.to_points(self.editor.cursor))
            .is_some_and(|layer| layer.order != egui::Order::Background);
        let ui_has_pointer = gfx.egui_ctx.egui_is_using_pointer()
            || over_area
            || !self.editor.pointer_over_canvas(self.editor.cursor);
        let pivot = self.editor.canvas_pivot;

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

            // Dropping a file on the window is the gesture people already have
            // for this, and it reaches exactly the same importer the File menu
            // does — including its refusals, so an unsupported format explains
            // itself here too rather than silently doing nothing.
            WindowEvent::DroppedFile(path) => self.open_path(&path),

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
                        let point = self.editor.sample(pos, None);
                        self.editor.stroke.extend(point);
                    }
                    Interaction::Panning => {
                        let delta = pos - self.editor.last_cursor;
                        self.editor.camera.pan_by_screen(delta);
                    }
                    Interaction::Zooming => {
                        // Horizontal drag zooms about where the drag started,
                        // which is the convention every other paint app uses.
                        let dx = pos.x - self.editor.last_cursor.x;
                        let factor = 1.008f32.powf(dx);
                        let anchor = self.editor.zoom_anchor;
                        self.editor.camera.zoom_at(anchor, factor, pivot);
                    }
                    Interaction::Idle => {}
                }
                if self.editor.interaction != Interaction::Idle
                    && let Some(g) = self.gfx.as_ref()
                {
                    g.window.request_redraw();
                }
            }

            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = state == ElementState::Pressed;
                // Middle-drag and space-drag always pan, whatever tool is
                // selected — muscle memory should not depend on the rail.
                let pan_override = button == MouseButton::Middle
                    || (button == MouseButton::Left && self.editor.space_down);

                if pan_override {
                    self.editor.interaction = if pressed {
                        Interaction::Panning
                    } else {
                        Interaction::Idle
                    };
                } else if button == MouseButton::Left {
                    if pressed && !ui_has_pointer && self.modifiers.alt_key() {
                        // Alt is the eyedropper in every paint app; honouring it
                        // whatever tool is selected is what people expect.
                        self.pick_colour_at_cursor();
                    } else if pressed && !ui_has_pointer {
                        let pos = self.editor.cursor;
                        self.editor.last_cursor = pos;
                        match self.editor.ui.tool {
                            Tool::Brush | Tool::Eraser => {
                                let point = self.editor.sample(pos, None);
                                self.start_stroke(point);
                            }
                            Tool::Pan => self.editor.interaction = Interaction::Panning,
                            Tool::Zoom => {
                                self.editor.zoom_anchor = pos;
                                self.editor.interaction = Interaction::Zooming;
                            }
                        }
                    } else if !pressed {
                        match self.editor.interaction {
                            Interaction::Drawing => self.finish_stroke(),
                            _ => self.editor.interaction = Interaction::Idle,
                        }
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
                    .zoom_at(self.editor.cursor, factor, pivot);
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
                            let point = self.editor.sample(pos, reported);
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
                            let point = self.editor.sample(pos, reported);
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

/// The file's own name, for a message written to the user.
fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Make sure a chosen path ends in the extension Umber saves with.
///
/// Not every platform's save dialog appends the filter's extension, and a
/// document written as plain `sketch` would never open again: `docimport`
/// dispatches on the extension, so a file with none is refused by name before
/// it is ever read.
///
/// The suffix is appended rather than substituted. `with_extension` would turn
/// a deliberate `sketch.v2` into `sketch.ora`, quietly renaming the file the
/// user asked for.
fn with_extension(path: PathBuf) -> PathBuf {
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(docformat::EXTENSION))
    {
        return path;
    }
    let mut name = path.into_os_string();
    name.push(".");
    name.push(docformat::EXTENSION);
    PathBuf::from(name)
}

/// Write straight-alpha RGBA8 out as a PNG.
fn write_png(
    path: &std::path::Path,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    // The composite shader gamma-encodes on the way out, so the bytes are sRGB.
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    encoder.write_header()?.write_image_data(pixels)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_file_always_ends_up_openable() {
        // `docimport` dispatches on the extension, so a document saved as plain
        // `sketch` would be refused by name the next time it was opened.
        assert_eq!(with_extension("sketch".into()), PathBuf::from("sketch.ora"));
        assert_eq!(
            with_extension("sketch.ora".into()),
            PathBuf::from("sketch.ora"),
            "an extension already there must not be doubled"
        );
        assert_eq!(
            with_extension("sketch.ORA".into()),
            PathBuf::from("sketch.ORA"),
            "the case of an extension is not Umber's to change"
        );
        assert_eq!(
            with_extension("sketch.v2".into()),
            PathBuf::from("sketch.v2.ora"),
            "the name the user typed must survive whole"
        );
    }
}
