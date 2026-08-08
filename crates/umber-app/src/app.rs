//! Window lifecycle, input translation and the frame loop.

use crate::canvasdlg;
use crate::crash;
use crate::editor::{self, Editor, Floating, Interaction, Tool};
use crate::gesture;
use crate::keylayout;
use crate::logo;
use crate::session::{DocId, DocumentState};
use crate::shortcuts::{self, Action};
use crate::splash::{self, Splash};
use crate::swapchain;
use crate::sysclip::{self, Paste};
use crate::syscursor;
use crate::tabs::{self, Notice};
#[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
use crate::taskbar;
use crate::theme;
use crate::thumbs;
use crate::ui;
use crate::update;
use glam::{UVec2, Vec2};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use umber_core::docformat::{self, SaveDocument, SaveLayer};
use umber_core::export;
use umber_core::history::PatchPiece;
use umber_core::{
    Brush, Clip, Color, Dab, Document, Edit, EditBody, EditKind, InputPoint, Jump, PixelPatch,
    PixelRect, SelectionOp, Transform,
};
use umber_render::{
    CanvasRenderer, CompositeParams, DabStyle, FloatParams, FloatSource, Gpu, ProbeParams,
};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::{Window, WindowId};

/// What one press of the zoom-in shortcut multiplies the camera by.
///
/// Deliberately a bigger step than a wheel notch's 1.12: a notch is one of
/// several the finger rolls through in a gesture, where a keypress is the whole
/// intent. Multiplicative rather than a ladder of fixed percentages, so it
/// composes with the wheel and the pinch instead of snapping away from them.
const ZOOM_KEY_STEP: f32 = 1.25;

/// Screen points the canvas moves for one notch of the wheel.
///
/// Points rather than pixels, so a scroll covers the same amount of the screen
/// on a HiDPI display as on an ordinary one.
const WHEEL_PAN_POINTS: f32 = 48.0;

/// What one notch of Ctrl and the wheel multiplies the zoom by.
const WHEEL_ZOOM_STEP: f32 = 1.12;

/// Pixels a trackpad reports for one notch's worth of scrolling.
///
/// Only the zoom needs this: it works in notches, and a trackpad reports a
/// distance. Panning uses the distance as it stands.
const WHEEL_PIXELS_PER_NOTCH: f32 = 60.0;

/// Whether the interface, rather than the document, owns a press at `screen`
/// (physical window pixels).
///
/// **Takes a position rather than reading `Editor::cursor`**, and that is the
/// whole point of it being a function of one. `cursor` is written by
/// `CursorMoved`, which is a *mouse* event; a pen on Windows Ink arrives as
/// `WindowEvent::Touch` through `WM_POINTER` and never produces one. Asking
/// this about the stale cursor tested every pen press against wherever the
/// mouse happened to be last — `(0, 0)` on a fresh launch, which is the menu
/// bar — so the press was ruled the interface's and dropped, and a tablet drew
/// nothing at all.
///
/// A free function because the caller holds `Graphics` mutably for the whole of
/// `window_event`, and the two things this needs are separate fields.
///
/// Three parts. This used to ask egui, via `response.consumed` and
/// `egui_wants_pointer_input()`. Both are built on
/// `Context::is_pointer_over_egui`, which since egui 0.35 answers *true
/// everywhere*: `CentralPanel` now consumes the root `Ui`'s cursor, so the
/// unused rect it tests against is empty by the end of the pass. With it true,
/// `egui_wants_pointer_input()` is true on every fresh press — and the press
/// that begins a stroke was being swallowed. So decide it here:
///
/// * `egui_is_using_pointer` — a slider or a scrollbar has the drag. The one
///   part of egui's answer that does not depend on the broken test, and the one
///   part that is not about a position.
/// * a non-background layer at `screen` — a menu, a popup, or a floating panel,
///   all of which are `Area`s and all of which sit over the canvas rather than
///   beside it.
/// * `pointer_over_canvas` — the canvas region itself, minus whatever the
///   layout and the scrollbars have claimed, computed from the same rect the
///   composite pass is given.
fn ui_owns_pointer(editor: &Editor, ctx: &egui::Context, screen: Vec2) -> bool {
    ctx.egui_is_using_pointer()
        || editor::over_egui_area(editor, ctx, screen)
        || !editor.pointer_over_canvas(screen)
}

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
    /// A reconfigure a frame asked for and could not carry out itself.
    ///
    /// Only ever set by a frame that kept its surface texture — a `Suboptimal`
    /// acquisition — because configuring while one is alive is refused by
    /// wgpu, fatally. See [`crate::swapchain`]; it is applied by
    /// [`Graphics::reconfigure_surface`] before the next acquisition.
    reconfigure_pending: bool,
}

impl Graphics {
    /// Configure the surface from `config`, and clear any pending request.
    ///
    /// The one route to `Surface::configure` after start-up, so "no surface
    /// texture may be alive here" is a property of one call site rather than
    /// of three. It also means a resize does not leave a deferred reconfigure
    /// behind it to be paid for a second time on the next frame.
    fn reconfigure_surface(&mut self) {
        self.surface.configure(&self.gpu.device, &self.config);
        self.reconfigure_pending = false;
    }

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
    /// The **resolved palette**, not the theme and accent it came from. The
    /// accent had to be in the key because it re-hues the palette and egui's
    /// own tokens carry it — keyed on the theme alone, picking a new accent
    /// left the selection fill and the hyperlink colour on the old hue until
    /// something else happened to trigger a restyle. A theme somebody is
    /// *editing* is the same argument taken one step further: its name does not
    /// change while its colours do, so anything short of the palette itself
    /// would leave the settings dialog showing the old colours while the cards
    /// beside it showed the new ones. It is 27 `Color32`s and it is compared
    /// once a frame.
    applied_theme: Option<theme::Palette>,
    bindings: Vec<shortcuts::Binding>,
    /// When egui next wants to be redrawn, if it does — a fading hover, a
    /// blinking caret. `None` means "sleep until something happens".
    ///
    /// Kept here rather than acted on inside [`Self::render`] because setting
    /// the control flow needs the [`ActiveEventLoop`], which only the handler
    /// methods are given.
    repaint_at: Option<std::time::Instant>,
    /// Where a press landed that was *outside* the floating transform's box, in
    /// physical window pixels — for as long as it might still turn out to have
    /// been a click rather than a rotation.
    ///
    /// `Transform::grab` answers `Handle::Rotate` everywhere outside the box, so
    /// the geometry can no longer say which of the two an outside press is; only
    /// what the pointer does next can. Cleared the moment it travels past
    /// [`PUT_DOWN_SLOP`], and until then no rotation is applied at all — a
    /// release that puts the picture down must not first have nudged it a
    /// degree.
    ///
    /// Here rather than on `Floating` because it is the *pointer's* state, in
    /// physical pixels, and the same distinction the editor draws between a
    /// gesture and a document. Nothing about it survives the release.
    put_down_at: Option<Vec2>,
    /// Umber's hold on the desktop's clipboard, for pictures.
    ///
    /// On the application rather than in `Editor` for the reason `gfx` is: it
    /// is a resource the process holds, not something a document has, and a tab
    /// switch has nothing to do with it. `Editor::clipboard` — the `Clip`
    /// itself — stays where it is, above the `--- documents ---` line, because
    /// copying out of one document and into another is most of what a clipboard
    /// is for.
    sysclip: sysclip::Board,
}

/// How far the pointer may travel from a press outside the transform box before
/// that press stops being "put it down" and becomes a rotation, in physical
/// window pixels.
///
/// Physical rather than points, and small: it is the hand's wobble on a click,
/// not a deliberate movement. Anything much larger and a short deliberate turn
/// would commit instead of turning.
const PUT_DOWN_SLOP: f32 = 4.0;

/// Submit the frame's commands, and only then destroy the textures egui has
/// finished with.
///
/// The two are one function because putting them the other way round is a
/// crash, and a crash that reads like somebody else's bug.
/// `egui_wgpu::Renderer::free_texture` calls `wgpu::Texture::destroy`, which
/// takes effect **immediately** rather than when the last reference to the
/// texture goes: from that moment every recorded command naming it is invalid.
///
/// And a texture may legitimately be freed in the same pass that draws it. egui
/// frees one when the last `TextureHandle` to it is dropped, and a cache
/// replacing an entry mid-pass does exactly that — after an earlier widget has
/// already queued a `Shape` carrying the id. So this frame's paint jobs can and
/// do reference what this frame's `textures_delta.free` names. Destroying first
/// meant `Queue::submit` failing validation with "Texture with
/// 'egui_texid_Managed(N)' label has been destroyed", which under wgpu's
/// default handler is a panic that takes the application down with it. Opening
/// the brush library was enough: the Brushes panel and the browser draw the
/// same preset at two different row heights, and the second evicted the first's
/// preview texture after it had been painted.
///
/// After the submit it is safe, and needs no deferring by a frame: wgpu keeps
/// the underlying resource alive for as long as the submission using it. This
/// is what `egui_wgpu`'s own painter does, for the same reason, stated in the
/// same place.
///
/// **`crash::window` calls this too**, which is why it is `pub(crate)` and free
/// rather than a method: the crash reporter is a second process drawing a
/// second egui pass, it had its own copy of the ordering, and its copy was the
/// wrong way round. Anything given to this function must stay reachable from a
/// process with no `Editor`, no document and no `CanvasRenderer` — a `Graphics`
/// parameter, or anything that reads editor state, breaks the window whose
/// whole job is to survive Umber having stopped.
pub(crate) fn submit_frame(
    gpu: &Gpu,
    renderer: &mut egui_wgpu::Renderer,
    encoder: wgpu::CommandEncoder,
    finished: &[egui::TextureId],
) {
    gpu.queue.submit(Some(encoder.finish()));
    release_finished_textures(renderer, finished);
}

/// Hand egui's finished textures back.
///
/// Only ever called with nothing recorded and unsubmitted. [`submit_frame`] is
/// the one caller that has a command buffer, and it submits first; the two
/// direct callers — `UmberApp::render` and `crash::window`, each on the path
/// where the surface gave them nothing to draw into — have not created an
/// encoder yet. Calling it anywhere a frame's commands are recorded and
/// unsubmitted is the crash [`submit_frame`] describes.
pub(crate) fn release_finished_textures(
    renderer: &mut egui_wgpu::Renderer,
    finished: &[egui::TextureId],
) {
    for id in finished {
        renderer.free_texture(id);
    }
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
            put_down_at: None,
            sysclip: sysclip::Board::default(),
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
            // The style the stroke was begun with, not a fresh reading. It is
            // the same snapshot the preview and the commit are handed, so the
            // dab pipeline and `StrokeStyle::per_dab_color` cannot disagree
            // about whether a colour was recorded — the thing that has to hold
            // for every frame of a stroke. It is also where a coloured *stamp*
            // joins pickup and colour modulation, which `StrokeBuilder` cannot
            // see because a tip is a name the editor resolves.
            per_dab_color: self.editor.stroke_style.per_dab_color,
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

        // The parts of that rectangle the dabs actually reached. A stroke's
        // bounding box is a dreadful description of a diagonal — 381 MB to
        // record a few million pixels on a 10000² canvas — so the patch and the
        // commit are both cut to these, and to the *same* ones: what is
        // committed and what was captured have to be the same pixels or an undo
        // does not undo.
        let pieces = self.editor.stroke.damage().pieces(rect);

        // Capture undo state first. The readback submits and blocks on its own
        // encoder, so it observes the layer before `enc` commits anything.
        let before = canvas.read_layer_pieces(&gfx.gpu.device, &gfx.gpu.queue, slot, &pieces);
        let captured = pieces
            .iter()
            .zip(before)
            .map(|(rect, bytes)| PatchPiece::new(*rect, bytes))
            .collect();
        // Labelled from the *snapshotted* style rather than from the brush in
        // hand, for the same reason the commit is: switching tool mid-stroke
        // must not change what the stroke that is ending turns out to have
        // been, in the history list any more than on the canvas.
        self.editor.history.record(Edit::new(
            EditKind::for_mode(self.editor.stroke_style.mode),
            PixelPatch::from_pieces(rect, slot, captured),
        ));

        canvas.commit_stroke(
            &gfx.gpu.device,
            &gfx.gpu.queue,
            &mut enc,
            slot,
            rect,
            &pieces,
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
        // A locked layer refuses here, once, inside `begin_stroke` — every
        // route to a stroke passes through it. Nothing below runs, so the
        // pointer goes on producing move events that reach a stroke builder
        // that was never begun and does nothing with them.
        if !self.editor.begin_stroke(point) {
            return;
        }

        let id = self.editor.session.active_id();
        let tip = self.editor.tip.clone();
        if let Some(gfx) = self.gfx.as_mut()
            && let Some(canvas) = gfx.canvases.get_mut(&id)
        {
            // Cheap when the brush has not changed: `set_tip` compares the mask
            // by identity and returns without touching the GPU.
            //
            // Whether the tip's *own* colour is stamped is decided in
            // `begin_stroke`, above, and handed over rather than re-derived —
            // it is the same answer `StrokeStyle::per_dab_color` was built
            // from, and the two must not be able to disagree. An eraser and a
            // stroke on a mask both say no.
            canvas.set_tip(
                &gfx.gpu.device,
                &gfx.gpu.queue,
                tip,
                self.editor.stroke_stamps_colour,
            );

            // The selection, on exactly the same footing and for the same
            // reasons: one binding covers a whole dab pass, and a selection
            // changed mid-stroke would leave the coverage already in the
            // scratch clipped by one that has gone. Compared by `Arc`
            // identity, so an unchanged selection costs a pointer comparison —
            // and this is also what re-binds it after a tab switch or an
            // Android resume, where the renderer is a different object.
            let selection = self.editor.selection.clone();
            canvas.set_selection(&gfx.gpu.device, &gfx.gpu.queue, selection);

            // The paper, on the same footing and for the same reasons: one
            // binding per pass, changed only between strokes. Read off the
            // *snapshotted* brush, so changing the Texture sliders mid-stroke
            // cannot re-texture the half already painted.
            // Which tile is `Editor::paper_tile`'s answer and nobody else's:
            // the brush may name one out of the user's library, in which case
            // `grain_pattern` says nothing about it. A name that resolves to
            // nothing binds no grain, which is the exact identity.
            let grain = self.editor.stroke.grain().and_then(|(strength, scale)| {
                self.editor.paper_tile().map(|tile| (tile, strength, scale))
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
        // Put the floating picture down first. Undo writes straight into the
        // layer, and a preview standing in front of it would go on showing the
        // state the undo just replaced — and would then commit back over it.
        // Here rather than at the call sites: the keyboard, the History
        // module's rows and `jump_history` all reach this, and one of the three
        // would have been forgotten.
        self.finish_transform();
        if !self.canvas_is_ready() {
            return;
        }
        // The history is the live document's own, so this can only ever undo
        // work done on the canvas the user is looking at.
        let Some(edit) = self.editor.history.take_undo() else {
            return;
        };
        let inverse = self.reverse(edit.kind, edit.body);
        // The label and the time travel with the entry rather than being
        // recomputed, so an undone stroke keeps its name and the moment it was
        // painted on the far side of the cursor — the list neither renumbers
        // nor re-times itself as it is stepped through.
        self.editor
            .history
            .push_redo(Edit::made_at(edit.kind, edit.at, inverse));
        self.editor.mark_modified();
    }

    fn redo(&mut self) {
        self.finish_transform();
        if !self.canvas_is_ready() {
            return;
        }
        let Some(edit) = self.editor.history.take_redo() else {
            return;
        };
        let inverse = self.reverse(edit.kind, edit.body);
        self.editor
            .history
            .push_undo(Edit::made_at(edit.kind, edit.at, inverse));
        self.editor.mark_modified();
    }

    /// Does the document in front have GPU storage to be undone into?
    ///
    /// Asked *before* the entry is taken off the stack, because taking one and
    /// then finding there is nowhere to apply it would lose it.
    fn canvas_is_ready(&self) -> bool {
        let id = self.editor.session.active_id();
        self.gfx
            .as_ref()
            .is_some_and(|gfx| gfx.canvases.contains_key(&id))
    }

    /// Carry out one entry backwards, and return what putting it back would be.
    ///
    /// The two bodies reverse in genuinely different ways, and this is the one
    /// place that knows it:
    ///
    /// * A patch is swapped for the pixels it replaces, so the entry that goes
    ///   on the other stack holds what was there a moment ago.
    /// * A structural entry is swapped for the shape the stack has now, which
    ///   is the same move one level up and **touches no pixels at all**. A
    ///   layer this takes out of the stack travels inside the entry that goes
    ///   on the other stack, holding its texture slice, so nothing else can be
    ///   given that slice and every recorded patch naming it goes on meaning
    ///   the pixels it was captured from.
    /// * A flip is **its own inverse**, so it is carried out again and the
    ///   entry that goes on the other stack is the same nothing. This is what
    ///   the history's whole flip design rests on: no coordinate mapping, no
    ///   mirrored bytes, and every older patch reached with the canvas already
    ///   back in the orientation it was recorded in.
    fn reverse(&mut self, kind: EditKind, body: EditBody) -> EditBody {
        match body {
            EditBody::Pixels(patch) => {
                let id = self.editor.session.active_id();
                let Some(gfx) = self.gfx.as_mut() else {
                    return EditBody::Pixels(patch);
                };
                let Some(canvas) = gfx.canvases.get_mut(&id) else {
                    return EditBody::Pixels(patch);
                };
                // The pieces of the patch, not its bounding box: swapping them
                // is what an undo *is*, and the pixels between them were never
                // touched.
                EditBody::Pixels(swap_patch(canvas, &gfx.gpu, &patch))
            }
            EditBody::Structure(shape) => {
                let back = self.editor.layers.restore_shape(*shape);
                // Stepping over an "Add mask" takes the slice away while the
                // switch still says Mask. `Editor::stroke_target` already falls
                // back to the layer, so nothing downstream sees an impossible
                // state; what this stops is the *control* showing one.
                if self.editor.layers.active_mask().is_none() {
                    self.editor.edit_target = umber_core::EditTarget::Layer;
                }
                EditBody::Structure(Box::new(back))
            }
            EditBody::Flip => {
                if let Some(axis) = kind.flip_axis() {
                    self.mirror_document(axis);
                }
                EditBody::Flip
            }
        }
    }

    /// Mirror every layer's pixels and the selection with them.
    ///
    /// The one route, shared by the command and by stepping over it in either
    /// direction — a flip is its own inverse, so a second implementation for
    /// the undo would be a second thing to keep exact.
    ///
    /// Returns false when the document has no GPU storage, in which case
    /// nothing at all was mirrored and the caller must not record an entry
    /// saying otherwise.
    fn mirror_document(&mut self, axis: umber_core::FlipAxis) -> bool {
        // **The one gate a lock has on the flip**, on the way out as well as on
        // the way back, since undoing a flip comes through here too. Refused
        // *whole* rather than applied to the unlocked layers: a picture with
        // some layers mirrored and some not is one that was never on screen,
        // and a flip that half happened cannot be undone by flipping again,
        // which is the entire reason it stores no pixels. The menu item is
        // disabled to match — see `ui::draw`.
        if self.editor.layers.any_locked() {
            return false;
        }
        // Masks are slices too, and a mask that stayed put while its layer
        // mirrored would hide the wrong half of it.
        let slots: Vec<u32> = self
            .editor
            .layers
            .layers()
            .iter()
            .flat_map(|l| [l.slot(), l.mask()])
            .flatten()
            .collect();
        let id = self.editor.session.active_id();
        let Some(gfx) = self.gfx.as_mut() else {
            return false;
        };
        let Some(canvas) = gfx.canvases.get_mut(&id) else {
            return false;
        };
        canvas.flip_layers(&gfx.gpu.device, &gfx.gpu.queue, &slots, axis);
        self.editor.flip_canvas(axis);
        true
    }

    /// Mirror the whole document, and record it.
    ///
    /// Photoshop's Image ▸ Flip Canvas: the pixels of every layer, not the
    /// view. The canvas size is unchanged, which is exactly why this does not
    /// clear the undo history the way a resize does — every patch already in it
    /// is a rectangle of a canvas that still exists, and the entry recorded
    /// here is what puts the orientation back before any of them is reached.
    fn flip_canvas(&mut self, axis: umber_core::FlipAxis) {
        // The floating pixels are in no layer, so they would not be mirrored
        // with the picture and would then be put down in the place they were
        // dragged to before the flip. Put them down first, as every path that
        // leaves the document behind does.
        self.finish_transform();
        // The scratch surface is not mirrored either, so a stroke still in
        // flight would commit unmirrored over a flipped picture.
        self.finish_stroke();
        let id = self.editor.session.active_id();
        // A capture part-way through would assemble a file out of layers that
        // were mirrored and layers that were not. `flip_layers` cancels the
        // renderer's half; this is the scheduler's.
        self.stop_autosave_of(id);

        if !self.mirror_document(axis) {
            return;
        }
        // No pixels: undoing this is flipping again. See
        // `umber_core::history::EditBody`.
        self.editor
            .history
            .record(Edit::new(EditKind::for_axis(axis), EditBody::Flip));
        self.editor.mark_modified();
        self.request_redraw();
    }

    // --- the transform tool -------------------------------------------------

    /// Which layer slice the composite pass must be shown a preview slice for
    /// instead, if a transform is floating over the document in front.
    ///
    /// One call rather than a field, because the answer is the renderer's and a
    /// second copy of it could go stale — a resize or a tab switch ends a float
    /// on the renderer's side.
    fn float_preview(&self) -> Option<(u32, u32)> {
        let id = self.editor.session.active_id();
        self.gfx.as_ref()?.canvases.get(&id)?.float_preview()
    }

    /// Everything a float needs from the GPU, in one place.
    ///
    /// `pixels` is `Some` for a paste and `None` for a lift out of the layer;
    /// that is also what decides whether the commit has a hole to restore.
    /// Returns false when there was no room, which is when the layer stack is
    /// already using every slice the composite shader's array has.
    fn begin_float(&mut self, rect: PixelRect, pixels: Option<&[u8]>) -> bool {
        // **The one gate a lock has on the transform tool.** A lift and a paste
        // both come through here, so neither needs a check of its own — and
        // neither does the drag, the commit or the flip buttons, because
        // without a float there is nothing for any of them to act on.
        //
        // A *paste* says so and a lift does not, which is not an inconsistency:
        // a paste is an explicit command with one obvious outcome, where a
        // press on the canvas with the transform tool in hand happens every
        // time somebody puts the pen down. A notice raised by that would be a
        // dialog appearing over the canvas repeatedly, which is the failure the
        // autosave's "say it once" rule is about.
        if self.editor.layers.active_is_locked() {
            if pixels.is_some() {
                self.editor.notice = Some(Notice {
                    title: "The layer is locked".to_string(),
                    lines: vec![
                        "Nothing was pasted. Unlock the layer in the Layers panel, or \
                         select another one, and paste again."
                            .to_string(),
                    ],
                });
            }
            return false;
        }
        // A folder holds no pixels, so there is nothing to lift out of it and
        // nowhere to paste into it. Refused at the same one gate the lock is,
        // and silently for the same reason a locked *canvas press* is: this is
        // reached every time the pen goes down with the transform tool in hand.
        let Some(slot) = self.editor.layers.active_slot() else {
            if pixels.is_some() {
                self.editor.notice = Some(Notice {
                    title: "A folder is selected".to_string(),
                    lines: vec![
                        "Nothing was pasted. A folder holds no pixels. Select a \
                         layer in the Layers panel and paste again."
                            .to_string(),
                    ],
                });
            }
            return false;
        };
        // The preview takes the slice one past the highest one claimed, which
        // is above every parked slice by construction — so a float can never be
        // rendered into a deleted layer's pixels. That also means a history
        // holding parked layers pushes the number up, and this release is what
        // stops an ordinary session of adding and deleting walking it to the
        // ceiling and refusing every transform from then on.
        //
        // `free_headroom` and not `free_a_slot`: what a preview needs is a
        // slice *above everything*, which a pool holding a gap in the middle
        // does not have even though it can hand one out. Eager rather than
        // after a refusal, unlike the two above, because the refusal it exists
        // to prevent is the only one left by this point — the lock and the
        // folder are already answered above.
        self.free_headroom();
        let reserved = self.editor.layers.slot_capacity_needed();
        // A lift is clipped by the selection; a paste puts down exactly what it
        // was given, having been masked when it was copied.
        let mask = pixels
            .is_none()
            .then(|| self.editor.selection.clone())
            .flatten();
        let id = self.editor.session.active_id();

        let started = {
            let Some(gfx) = self.gfx.as_mut() else {
                return false;
            };
            let Some(canvas) = gfx.canvases.get_mut(&id) else {
                return false;
            };
            canvas
                .begin_float(
                    &gfx.gpu.device,
                    &gfx.gpu.queue,
                    reserved,
                    &FloatSource {
                        slot,
                        rect,
                        pixels,
                        mask: mask.as_deref(),
                    },
                )
                .is_some()
        };
        if !started {
            self.editor.notice = Some(Notice {
                title: "Nothing was picked up".to_string(),
                // **Not "delete a layer".** That used to free a slice and now
                // parks one, in the undo entry that could put the layer back —
                // so the advice would have made the refusal worse, which is the
                // shape of lying control this project refuses everywhere. What
                // does free them is what got here: the history has already
                // given up every entry it could, so the only slices left are
                // ones the live stack is using.
                // Nothing here promises a remedy that may not work. The earlier
                // wording said "deleting a layer will free one", which stopped
                // being true the day a delete started parking its slice in the
                // undo entry; a later draft promised a second try, which is
                // only true when the slice that comes free happens to be the
                // one at the top of the range.
                lines: vec![
                    // No figure: it used to say "of 129", and the ceiling now
                    // carries headroom for effect slices, so a count naming it
                    // is a number the painter cannot check against anything on
                    // screen.
                    //
                    // And no "fewer layers, or fewer masks", which is what this
                    // said until the comment above was read against it. That is
                    // precisely the remedy that does not work — a delete and a
                    // `remove_mask` both *park* the slice — so it was the
                    // control-that-lies this project refuses, in the one place
                    // somebody is already stuck. Reopening genuinely works: the
                    // stack is rebuilt from a fresh pool and the numbering
                    // packs back down from zero.
                    //
                    // **And it names what that costs**, because it costs
                    // something: `SaveHistory::new` skips structural entries,
                    // which are exactly the ones holding parked slices, so the
                    // reopened document cannot undo the deletes that got it
                    // here. An advice that works and quietly throws away undo
                    // history is the same class of failure as one that does
                    // not work — the artist has to be able to weigh it.
                    "A transform needs a spare texture slice to preview into, and \
                     this document is using every one Umber has. Saving and \
                     reopening the document will pack them back down, though \
                     deleted layers cannot be brought back afterwards."
                        .to_string(),
                ],
            });
            return false;
        }
        self.editor.float = Some(Floating {
            xf: Transform::identity(rect),
            slot,
            lifted: pixels.is_none(),
            drag: None,
        });
        true
    }

    /// A press on the canvas with the transform tool in hand.
    ///
    /// With no float up, a press inside the region picks it up and the same
    /// press starts dragging it — one gesture, as it would be with any other
    /// tool. With one up, a *click* outside the box puts the pixels down; the
    /// next press picks up again.
    ///
    /// "Outside the box" is also where a rotation is grabbed, and the two are
    /// the same press. Which it was is settled at the release, by whether the
    /// pointer travelled — see [`PUT_DOWN_SLOP`]. That is why this cannot be
    /// decided in `umber-core`: `Transform::grab` sees one position, and the
    /// difference between a click and a drag is not in it.
    fn transform_press(&mut self, screen: Vec2) {
        let doc = self.editor.screen_to_doc(screen);
        self.put_down_at = None;
        if self.editor.float.is_some() {
            // `Handle::Rotate` is returned for outside the quad and nowhere
            // else, so this needs no second geometry test of its own.
            if self.editor.transform_press(doc) == Some(umber_core::Handle::Rotate) {
                self.put_down_at = Some(screen);
            }
            return;
        }
        if !self.editor.transform_would_grab(doc) {
            return;
        }
        // A stroke still in flight belongs to the layer this is about to lift
        // out of, and would otherwise be baked in underneath the hole.
        //
        // `pointer_pressed` has already finished it on the one route that
        // reaches here today — `gesture::supersedes_stroke` answers true for
        // `Press::Transform` — so this is a no-op on that path. It stays
        // because the rule it states is `begin_float`'s and not the pointer's:
        // no float may be lifted with a stroke in flight, whatever route got
        // here. Delete the other one, not this one.
        self.finish_stroke();
        let rect = self.editor.transform_region();
        if self.begin_float(rect, None) {
            self.editor.transform_press(doc);
        }
    }

    /// Put the floating pixels down, recording the edit.
    ///
    /// The undo patch is captured here rather than when the pixels were picked
    /// up, for exactly the reason `finish_stroke`'s is: the layer is untouched
    /// until this moment, so reading it now yields the pre-transform pixels —
    /// and only now is the damaged rectangle known.
    fn finish_transform(&mut self) {
        // Whatever route got here — Enter, a tab switch, a save — there is no
        // box left for a pending put-down to be outside of.
        self.put_down_at = None;
        let Some(float) = self.editor.float.take() else {
            return;
        };
        let doc = self.editor.doc.size;
        let params = FloatParams {
            inverse: float.xf.inverse(),
            dest: float.xf.dest_rect(doc),
        };
        // A lift that never moved puts back exactly what it took, so there is
        // nothing to write and nothing to name in the history. A *paste* is
        // never in that case even at identity: its pixels were not there
        // before.
        let unchanged = float.lifted && float.xf.is_identity();
        let damage = float.xf.damage(doc, float.lifted).filter(|_| !unchanged);

        let id = self.editor.session.active_id();
        let Some(gfx) = self.gfx.as_mut() else {
            return;
        };
        let Some(canvas) = gfx.canvases.get_mut(&id) else {
            return;
        };
        let Some(damage) = damage else {
            canvas.end_float();
            return;
        };

        // Blocks on the GPU, and is submitted on its own encoder so it observes
        // the layer before the commit below touches it. Once per gesture, at
        // pointer-up, exactly as a stroke's is.
        let before = canvas.read_layer_rect(&gfx.gpu.device, &gfx.gpu.queue, float.slot, damage);
        self.editor.history.record(Edit::new(
            EditKind::Transform,
            PixelPatch::new(damage, float.slot, before),
        ));

        let mut enc = gfx
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("finish-transform"),
            });
        canvas.commit_float(&gfx.gpu.queue, &mut enc, damage, &params);
        gfx.gpu.queue.submit(Some(enc.finish()));
        canvas.end_float();

        // The marquee follows the picture it described. Only for a lift: a
        // paste did not come out of the selection, so moving it would be a
        // claim about the artist's intent that nothing supports.
        if float.lifted {
            self.editor.carry_selection(&float.xf);
        }
        self.editor.mark_modified();
    }

    /// The pointer moved with a transform handle held, in physical pixels.
    ///
    /// The half of the click-or-rotation question that watches the travel. A
    /// press outside the box turns nothing at all until it has moved past
    /// [`PUT_DOWN_SLOP`]; once it has, the drag is applied from the point it was
    /// grabbed at — `Transform::drag` is absolute against that point, so no part
    /// of the rotation is lost by having waited.
    fn transform_moved(&mut self, screen: Vec2, uniform: bool) -> bool {
        if let Some(from) = self.put_down_at {
            if from.distance(screen) <= PUT_DOWN_SLOP {
                return false;
            }
            self.put_down_at = None;
        }
        let doc = self.editor.screen_to_doc(screen);
        self.editor.transform_moved(doc, uniform)
    }

    /// A transform handle let go of.
    ///
    /// The float itself stays up — the box is still there to be dragged again —
    /// unless the gesture turned out to be a click outside it, which is the one
    /// reading that puts the pixels down.
    fn transform_release(&mut self) {
        let put_down = self.put_down_at.take().is_some();
        self.editor.transform_release();
        if put_down {
            self.finish_transform();
        }
    }

    /// Abandon a floating transform. The layer was never written to, so this is
    /// only giving the storage back.
    fn cancel_transform(&mut self) -> bool {
        self.put_down_at = None;
        if self.editor.float.take().is_none() {
            return false;
        }
        let id = self.editor.session.active_id();
        if let Some(gfx) = self.gfx.as_mut()
            && let Some(canvas) = gfx.canvases.get_mut(&id)
        {
            canvas.end_float();
        }
        self.request_redraw();
        true
    }

    /// Settle the document and answer what a copy or a cut should act on: the
    /// rectangle to read, and the mask that clips it.
    ///
    /// **A float in hand is put down first, and that is one rule for all three
    /// of copy, cut and paste.** The clipboard is a picture of the document, and
    /// while pixels are in the air the document has no definite state to take a
    /// picture of — the same reason every path that leaves it (a tab switch, a
    /// save, an export, a resize, a close) commits first. Once committed those
    /// pixels *are* the document, so what follows needs no special case.
    ///
    /// **And it deliberately gets none.** A float that arrived by *paste* did
    /// not come out of the selection, so after the commit the marquee names
    /// somewhere else and a copy answers "nothing to copy". Reading the float's
    /// destination rectangle instead is the obvious repair and is wrong, in a
    /// way that only shows up on the cut: `Transform::dest_rect` is the bounding
    /// box of the **quad**, plus a skirt, and a rectangle is not the shape of
    /// the picture. Cutting it with no mask clears every pixel in that box — the
    /// four corners left over by a rotation, whatever showed through the clip's
    /// own transparency, and the skirt — none of which the paste ever covered.
    /// That is silent damage to the layer, recorded as one entry, and worse than
    /// a Ctrl+C that does nothing. The clipboard already holds what was pasted,
    /// which is what makes the missing case cheap; putting it back needs the
    /// clip's own alpha as a mask, not its bounding box.
    fn take_region(&mut self) -> (PixelRect, Option<Arc<umber_core::Selection>>) {
        self.finish_transform();
        self.finish_stroke();
        // A *lift* carried the marquee with it at commit, so after that the
        // selection is already over the pixels that were being held.
        (
            self.editor.transform_region(),
            self.editor.selection.clone(),
        )
    }

    /// Take the selection — or the whole layer where there is none — onto
    /// Umber's clipboard, and offer it to the rest of the machine.
    ///
    /// `read_layer_rect` blocks, which is acceptable here for the same reason
    /// it is acceptable in a save: this is an explicit action and is nowhere
    /// near the drawing loop. So does the write to the desktop's clipboard,
    /// which encodes a PNG, and it is not threaded — see `sysclip`'s module
    /// docs, where the ordering against the next paste is the deciding
    /// argument.
    ///
    /// **Both clipboards are written, and Umber's own is the one that must not
    /// fail.** The desktop's is best effort — a machine with no clipboard, or
    /// an allocation it refuses — but what became of it is *remembered* rather
    /// than only logged, because a refusal leaves the desktop holding an older
    /// picture and `sysclip::decide` would otherwise believe that one over the
    /// region just copied. Copy and paste inside Umber are unaffected either
    /// way, which is what makes the failure survivable rather than invisible.
    ///
    /// What is remembered is a *picture*, not a flag, and on a platform whose
    /// clipboard does not hand back the bytes it was given it is read straight
    /// back to get one. That second read is the echo; `sysclip` has the whole
    /// argument, including which platforms pay for it and why the answer is a
    /// `const` rather than a `cfg`.
    fn copy_selection(&mut self) {
        // `take_region` puts any float down and answers for whichever state the
        // copy was asked in — that is what lets a copy mid-transform read the
        // float's own region rather than the selection it came from.
        let (rect, mask) = self.take_region();
        // Nothing to copy out of a folder: it holds no pixels of its own.
        let Some(slot) = self.editor.layers.active_slot() else {
            return;
        };
        let id = self.editor.session.active_id();

        let Some(gfx) = self.gfx.as_ref() else { return };
        let Some(canvas) = gfx.canvases.get(&id) else {
            return;
        };
        let bytes = canvas.read_layer_rect(&gfx.gpu.device, &gfx.gpu.queue, slot, rect);
        match umber_core::Clip::from_layer(rect, &bytes, mask.as_deref()) {
            Some(clip) => {
                log::info!("copied {} × {}", clip.size().x, clip.size().y);
                self.sysclip.put_image(&clip);
                self.editor.clipboard = Some(clip);
            }
            // Nothing under the selection. The previous clipboard is left
            // alone: wiping it because a copy landed on an empty patch of
            // canvas would lose whatever the artist actually meant to keep.
            None => log::info!("nothing to copy"),
        }
    }

    /// The same take, and then the pixels leave the layer.
    ///
    /// One readback serves three purposes — what goes on the clipboard, what is
    /// written back, and the undo patch — because the bytes read here *are* the
    /// pre-cut state of the rectangle. There is no second blocking read, and
    /// `Clip::cut_from_layer` is what guarantees the removal is the exact
    /// complement of what was taken rather than a second reading of the mask.
    ///
    /// **The entry is an `Erase`, and that is not a placeholder.** A cut
    /// removes coverage and undoes by putting a rectangle of pixels back, which
    /// is what an eraser stroke is and undoes as; `EditKind` carries a variant
    /// only where the engine can restore something, and two rows that undo
    /// identically must not have two names — the same rule that keeps a paste
    /// filed under Transform.
    ///
    /// **The patch is the rectangle, not the cells a mark reached**, because a
    /// cut has no `TileMask` to have accumulated one from. With nothing selected
    /// that rectangle is the whole canvas, so on the 10000² document the Undo
    /// section uses as its bound a bare Ctrl+X costs 400 MB and the 512 MB
    /// budget holds exactly one — the history before it ages out. That follows
    /// from the same rule a stroke across such a canvas follows and is said here
    /// rather than left to look like a fault.
    fn cut_selection(&mut self) {
        // **The one gate a lock has on cutting.** An explicit command with one
        // obvious outcome, so it says so, exactly as a paste onto a locked
        // layer does.
        if self.editor.layers.active_is_locked() {
            self.editor.notice = Some(Notice {
                title: "The layer is locked".to_string(),
                lines: vec![
                    "Nothing was cut. Unlock the layer in the Layers panel, or select \
                     another one, and cut again."
                        .to_string(),
                ],
            });
            return;
        }
        let (rect, mask) = self.take_region();
        // Nothing to cut out of a folder: it holds no pixels of its own, so
        // there is nothing to take and nothing to write back. Silent rather
        // than a notice, for the reason the copy above is — a folder is a
        // perfectly ordinary thing to have selected.
        let Some(slot) = self.editor.layers.active_slot() else {
            return;
        };
        let id = self.editor.session.active_id();

        // Mutable because writing a slice bumps its thumbnail revision — the
        // list has to redraw the row a cut just emptied.
        let Some(gfx) = self.gfx.as_mut() else { return };
        let Some(canvas) = gfx.canvases.get_mut(&id) else {
            return;
        };
        let bytes = canvas.read_layer_rect(&gfx.gpu.device, &gfx.gpu.queue, slot, rect);
        // `None` on exactly the terms a copy gets it: there was nothing under
        // the selection. The layer is then left alone rather than written with
        // a copy of itself and given a history entry that restores nothing.
        let Some(cut) = umber_core::Clip::cut_from_layer(rect, &bytes, mask.as_deref()) else {
            log::info!("nothing to cut");
            return;
        };
        canvas.write_layer_rect(&gfx.gpu.queue, slot, rect, &cut.remainder);
        log::info!("cut {} × {}", cut.clip.size().x, cut.clip.size().y);

        self.editor.history.record(Edit::new(
            EditKind::Erase,
            PixelPatch::new(rect, slot, bytes),
        ));
        // The desktop gets exactly what the copy would have given it: a cut is
        // a copy plus the removal, and `Clip::cut_from_layer` is what makes the
        // two halves the same take.
        self.sysclip.put_image(&cut.clip);
        self.editor.clipboard = Some(cut.clip);
        self.editor.mark_modified();
        self.request_redraw();
    }

    /// Put the clipboard down as a floating transform, ready to be moved.
    ///
    /// It arrives floating rather than committed, which is the whole reason the
    /// two features are one: a paste that had already been baked into the layer
    /// would have to be undone to be repositioned. That is as true of a
    /// screenshot off the desktop as it is of Umber's own copy — it arrives
    /// where it can be dragged, turned and scaled before it is anywhere.
    ///
    /// **Which of the two clipboards it comes off is `sysclip::decide`'s**, a
    /// pure function of what each is holding, so the rule is testable without a
    /// display server — which is the only way it is tested at all, because no
    /// test here may touch the real clipboard. Where a picture *goes* is
    /// `Clip::place`'s, in `umber-core`, and a foreign picture is an ordinary
    /// clip: there is deliberately no second placer for one.
    ///
    /// **A picture off the desktop is adopted only once it has actually been
    /// put down.** Adopting it up front is the obvious place and is wrong: a
    /// Ctrl+V on a locked layer or with a folder selected is refused by
    /// `begin_float`, and having already overwritten `Editor::clipboard` it
    /// would have thrown away the region the artist copied in Umber — to paste
    /// nothing. The desktop is unaffected by the refusal, so the next Ctrl+V
    /// finds the same picture again.
    ///
    /// The read blocks, which is what an explicit Ctrl+V may do and the drawing
    /// loop may not. It is not threaded, and the reason is in `sysclip`.
    fn paste(&mut self) {
        // The desktop is asked *first*, so a picture copied in another
        // application half a second ago is the one that lands. `on_desktop` is
        // what the desktop should be handing back for Umber's own clip, without
        // which a copy the desktop refused would be overruled by whatever it
        // held before, and a platform that does not return what it was given
        // would never have its own copy recognised — see `sysclip`.
        let taken = self.sysclip.take_image();
        let (clip, foreign) = match sysclip::decide(
            taken,
            self.editor.clipboard.as_ref(),
            self.sysclip.on_desktop(),
        ) {
            Paste::Nothing => return,
            Paste::Mine(clip) => (clip, false),
            Paste::Theirs(clip) => {
                log::info!(
                    "pasting {} × {} off the desktop's clipboard",
                    clip.size().x,
                    clip.size().y
                );
                (clip, true)
            }
        };
        if !self.float_a_clip(&clip, "pasted") {
            // Refused — off the canvas, or a locked layer, or a folder, which
            // `float_a_clip` has already said so about. Nothing has been
            // pasted, so nothing is adopted: Umber's own clipboard has to
            // survive a paste that did not happen, or Ctrl+V on the wrong layer
            // would throw away the region the artist copied. The desktop still
            // holds its picture, so the next Ctrl+V finds it again.
            return;
        }
        // Adopted only now, and only for a picture that came off the desktop:
        // a second Ctrl+V then puts down the same picture once the desktop's
        // clipboard has moved on, which is the rule that keeps Umber's own copy
        // alive when somebody copies a line of text. `note_adopted` is what
        // stops `decide` reading the adopted clip as one the desktop never
        // received.
        if foreign {
            self.editor.clipboard = Some(clip);
            self.sysclip.note_adopted();
        }
    }

    /// Put a rectangle of pixels on the canvas as a floating transform.
    ///
    /// The whole of what a paste does after it has decided *what* to put down,
    /// and therefore the whole of what placing a block of text does: `verb`
    /// is the only difference between the two, and it is a word in a sentence.
    /// Sharing it is what stops the crop notice, the centring rule and the
    /// switch to the transform tool being stated twice and drifting.
    ///
    /// False when nothing was put down — off the canvas entirely, a locked
    /// layer, or a folder selected. The last two have already raised a notice
    /// of their own inside `begin_float`.
    fn float_a_clip(&mut self, clip: &Clip, verb: &str) -> bool {
        self.finish_transform();
        self.finish_stroke();

        let doc = self.editor.doc.size;
        // Into the middle of the selection where there is one — "put it into
        // what I marked out" — and otherwise into the middle of what the artist
        // is looking at. Something that lands in a corner of the canvas nobody
        // is looking at appears to have done nothing.
        let centre = match self.editor.selection.as_ref() {
            Some(sel) => {
                let b = sel.bounds();
                Vec2::new(
                    b.x as f32 + b.width as f32 * 0.5,
                    b.y as f32 + b.height as f32 * 0.5,
                )
            }
            None => self
                .editor
                .camera
                .center
                .clamp(Vec2::ZERO, self.editor.doc.size_vec2()),
        };
        let Some(placed) = clip.place(doc, centre) else {
            log::info!("what was {verb} landed entirely off the canvas");
            return false;
        };
        if placed.rect.width < clip.size().x || placed.rect.height < clip.size().y {
            // Said out loud rather than logged, for the same reason an import
            // that loses something says so: silently cropping somebody's
            // picture is worse than refusing to.
            self.editor.notice = Some(Notice {
                title: format!("What was {verb} was cropped"),
                lines: vec![format!(
                    "It is {} × {} and this canvas is {} × {}, so only the middle of it \
                     reached the canvas. Enlarge the canvas under File → Canvas settings \
                     and try again to keep the rest.",
                    clip.size().x,
                    clip.size().y,
                    doc.x,
                    doc.y,
                )],
            });
        }
        if !self.begin_float(placed.rect, Some(&placed.pixels)) {
            return false;
        }
        // The box has handles, and they are the transform tool's. Landing in
        // another tool would leave a preview nothing could act on.
        self.editor.set_tool(Tool::Transform);
        self.request_redraw();
        true
    }

    /// Set the Text module's block and float it over the canvas.
    ///
    /// **Placed text is a paste**, and this is that sentence written out: the
    /// coverage becomes a `Clip` in the artist's own colour and goes through
    /// the same [`Self::float_a_clip`] Ctrl+V does. So it arrives with the
    /// transform tool's handles, Escape abandons it, a click outside puts it
    /// down, the undo entry is the `Transform` a paste already records, and the
    /// preview is byte for byte what commits — none of which is restated
    /// anywhere, and none of which reaches `umber-render`, `composite.wgsl` or
    /// the file format.
    ///
    /// Blocking: a font file is read and the glyphs are rasterised. That is
    /// what an explicit click may do and the drawing loop may not, exactly as
    /// the export and the blocking readback are.
    fn place_text(&mut self) {
        let setting = match self.editor.text.set() {
            Ok(setting) => setting,
            Err(err) => {
                // Every one of these is a finished sentence rather than a code:
                // the panel is where the artist was looking, and being told
                // "nothing happened" is the failure a notice exists to prevent.
                //
                // The sentences themselves are `textpanel::refusal`'s and there
                // is one copy of them, because the panel now says the same
                // things: it draws the refusal under the preview and disables
                // Place with it as the tooltip, since `build_preview` has
                // already set the real block and knows the exact error. This
                // notice stays as the belt to that gate's braces — the gate
                // catches the click, the notice catches a route that goes round
                // the gate, exactly as the lock is refused in both places.
                self.editor.notice = Some(Notice {
                    title: "Nothing was placed".to_string(),
                    lines: vec![crate::textpanel::refusal(err)],
                });
                return;
            }
        };
        let Some(clip) = setting.clip(self.editor.color) else {
            return;
        };
        self.float_a_clip(&clip, "set");
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
        // **The one gate a lock has on clearing.** The menu item is disabled to
        // match, so this only catches a shortcut.
        if self.editor.layers.active_is_locked() {
            return;
        }
        // Abandoned rather than committed: clearing the layer is the artist
        // saying they want none of it, and putting the floating pixels down
        // first only to wipe them would be theatre.
        self.cancel_transform();
        // A folder has nothing to clear. Deleting it is a different command and
        // is the one that would remove what is inside it.
        let Some(slot) = self.editor.layers.active_slot() else {
            return;
        };
        let id = self.editor.session.active_id();
        let Some(gfx) = self.gfx.as_mut() else { return };
        let Some(canvas) = gfx.canvases.get_mut(&id) else {
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

    /// Give the history's oldest entries up until the document has a texture
    /// slice to hand out, and say whether it now has.
    ///
    /// **The one release, in front of the three operations that take a slice**
    /// — a layer, a mask, a transform's preview — because the ceiling is now
    /// something a history competes for: a deleted layer's slice is parked in
    /// the entry that could put it back, so a session of adding and deleting
    /// can walk the pool empty where before a delete returned the number on the
    /// spot. It cannot live in `umber-render`, which is where the float's gate
    /// is and which cannot see a `History`.
    ///
    /// A `SlotRoom` rather than the stack itself, because the history is being
    /// mutated while the question is asked and the two are separate fields.
    ///
    /// **Called after the operation has been refused, never before it.**
    /// `free_until` gives nothing up while there is already room, so in the
    /// ordinary case this costs one lock and nothing else — but a layer can
    /// also be refused for reasons a released slice would not mend, a full
    /// stack most of all, and freeing first would throw an artist's oldest
    /// edits away to make room for something that was never going to happen.
    fn free_a_slot(&mut self) -> bool {
        let room = self.editor.layers.room();
        self.editor.history.free_until(move || room.has_room())
    }

    /// The same, for the one caller that needs a slice **above everything
    /// claimed** rather than merely a spare one.
    ///
    /// `CanvasRenderer::begin_float` takes its preview at
    /// `slot_capacity_needed`, which is what stops it rendering into a parked
    /// layer's pixels — so a pool holding a gap in the middle can satisfy
    /// `has_room` and still refuse the float. Asking the wrong question here is
    /// a valve that never opens: the history would answer "there is room" and
    /// give nothing up, and the transform tool would stay refused.
    ///
    /// **It refuses to spend the history where spending cannot help**, and that
    /// guard is not belt and braces. Unlike [`Self::free_a_slot`], which
    /// succeeds the moment it releases *any* claim, this one is satisfied only
    /// by releasing the claim at the **top** of the range — the tail is all
    /// `SlotPool::give_back` can compact. So where the live stack itself
    /// reaches the ceiling, no eviction whatever can help, and without the
    /// guard `free_until` would empty the undo stack, drain the redo stack and
    /// then answer false.
    ///
    /// **`live_slot_ceiling` is one past the highest slot *number* a live layer
    /// holds, not a count of live layers**, and reading it as a count is the
    /// mistake to avoid here. Parked slices push the numbering up and
    /// `SlotPool::give_back` compacts only the *tail*, so a layer created while
    /// most of the range is parked takes a number near the top and holds it
    /// there however much history is then given up. Two live layers are enough.
    /// This is why the guard is not made redundant by the slice ceiling being
    /// far above what a stack can claim on its own.
    ///
    /// The document this used to be described with was 64 layers each with a
    /// mask, and that was **already** wrong: 128 slices against the ceiling of
    /// 129 left `has_headroom` true, so the first `if` returned and the guard
    /// never ran. It is further from the ceiling now, at 256.
    fn free_headroom(&mut self) -> bool {
        let room = self.editor.layers.room();
        if room.has_headroom() {
            return true;
        }
        if self.editor.layers.live_slot_ceiling() >= umber_core::LayerStack::MAX_SLOTS {
            return false;
        }
        self.editor.history.free_until(move || room.has_headroom())
    }

    /// Would a new layer inside the selected folder be nested too deep?
    ///
    /// The second of `LayerStack::add`'s two refusals that a released slice
    /// cannot mend. Read here rather than asked of the stack, because it is a
    /// statement about what *this* add would do and the stack's own answer is
    /// the `None` we are already looking at.
    fn selected_folder_is_full(&self) -> bool {
        self.editor
            .layers
            .get(self.editor.layers.active_index())
            .is_some_and(|l| l.is_folder() && l.depth >= umber_core::LayerStack::MAX_DEPTH)
    }

    fn add_layer(&mut self) {
        // A new layer takes the next slot, which is the one a float would be
        // previewing into. Put the picture down before the two can collide.
        self.finish_transform();
        // Before the add, so a refusal records nothing. Every entry is `Kept`;
        // what makes this an undoable *add* is that the new layer is not among
        // them, so restoring this shape takes it back out — and the entry that
        // goes on the redo stack is the one that then holds it.
        let before = self.editor.layers.shape(self.editor.doc.layer_bytes());
        let slot = match self.editor.layers.add() {
            Some(slot) => slot,
            // A parked layer may be holding the last slice, so give the oldest
            // entries up and try once more — but **only where a slice is the
            // plausible reason**, which means excluding *both* of `add`'s other
            // refusals. A full stack is the obvious one: on a dry pool that is
            // exactly where releasing would throw an artist's oldest edits away
            // and then refuse anyway, since 64 masked layers is 128 slices and
            // the 64-entry cap at once. The second is a folder already at
            // `MAX_DEPTH`, which no released slice mends either.
            //
            // The shape is not re-taken. A release touches the history and the
            // pool and never the stack, so the snapshot is still the one this
            // add is about to change.
            None if self.editor.layers.len() < umber_core::LayerStack::MAX
                && !self.selected_folder_is_full()
                && self.free_a_slot() =>
            {
                let Some(slot) = self.editor.layers.add() else {
                    log::warn!("layer limit reached");
                    return;
                };
                slot
            }
            None => {
                log::warn!("layer limit reached");
                return;
            }
        };
        let needed = self.editor.layers.slot_capacity_needed();
        self.editor
            .history
            .record(Edit::new(EditKind::AddLayer, before));
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
        self.delete_entries(&[index]);
    }

    /// Run a reorder and record it, if it moved anything.
    ///
    /// The two chevrons share it so the shape is snapshotted before the move
    /// and the entry only recorded where one happened — a `MoveLayer` row for a
    /// drop that changed nothing would be a step the artist could click and see
    /// nothing undo. The drag in the layers panel does the same thing at its
    /// own call site, because it holds the `Editor` and not the `App`.
    fn record_move(&mut self, moved: impl FnOnce(&mut umber_core::LayerStack) -> bool) {
        let before = self.editor.layers.shape(self.editor.doc.layer_bytes());
        if !moved(&mut self.editor.layers) {
            return;
        }
        self.editor
            .history
            .record(Edit::new(EditKind::MoveLayer, before));
        self.editor.mark_modified();
    }

    /// Delete every ticked layer.
    ///
    /// Written in terms of [`Self::delete_entries`], like the single delete, so
    /// the lock gate, the float being put down and the history being cleared
    /// are each stated once — a second delete that forgot one of the three is
    /// exactly the bug the "one gate per operation" rule exists to prevent.
    fn delete_picked_layers(&mut self) {
        let targets = self.editor.layers.targets();
        self.delete_entries(&targets);
    }

    /// Delete a set of entries, and everything inside any folder among them.
    ///
    /// **One call to `remove_many`, never a loop of single deletes**, and that
    /// is a correctness rule rather than tidiness. A folder's contents sit
    /// *below* it, so deleting one shifts every index beneath it; a loop —
    /// even one walking backwards, which is what this used to be — hands the
    /// next iteration an index that now names a different entry. Ticking a
    /// layer and a group above it deleted a third layer nobody chose, and
    /// because a delete cleared the undo history it could not be taken back —
    /// it can now, which makes this the wrong thing to rely on rather than the
    /// only thing standing between the bug and the artist.
    /// `LayerStack::remove_many` resolves the whole set against the stack as it
    /// stands before anything moves.
    fn delete_entries(&mut self, indices: &[usize]) {
        // **The one gate a lock has on deletion.** A lock that stopped strokes
        // and let the layer be thrown away would protect nothing worth
        // protecting.
        //
        // Over the whole subtree of everything named, so a folder's lock
        // protects what is inside it and a locked layer inside an unlocked
        // group stops the *group* being deleted — because deleting a folder
        // deletes its contents, and half a deletion is not a state to leave a
        // stack in.
        let locked = indices.iter().any(|i| {
            self.editor
                .layers
                .subtree(*i)
                .any(|j| self.editor.layers.effective_locked(j))
        });
        if locked {
            return;
        }
        self.finish_transform();
        // Snapshotted before the removal, because what comes back names the
        // entries that are about to go and cannot hold them until the stack has
        // handed them over.
        let before = self.editor.layers.shape(self.editor.doc.layer_bytes());
        let Some(gone) = self.editor.layers.remove_many(indices) else {
            return;
        };
        // **This is the whole of why deleting a layer no longer clears the
        // history.** The removed layers travel into the entry, and each owns
        // its texture slice and its mask's, so neither number can be handed to
        // the next layer that asks: every recorded patch goes on meaning the
        // pixels it was captured from, and the deleted layer's slice is left
        // holding exactly the picture an undo would want to put back. No copy,
        // no readback, no GPU work at all.
        self.editor
            .history
            .record(Edit::new(EditKind::DeleteLayer, before.with_removed(gone)));
        self.editor.mark_modified();
    }

    /// Put the ticked layers — or the selected one — into a new group.
    ///
    /// Costs the undo history nothing to record, for exactly the reason
    /// reordering does: no slot changes hands. A folder holds none at all, and
    /// the layers moving into it keep the slices they always had, so the entry
    /// is a shape and a name and nothing else.
    fn group_layers(&mut self) {
        // The float previews into a spare slice and is anchored to a layer's
        // slot; grouping moves that layer. Put it down first, exactly as adding
        // a layer does.
        self.finish_transform();
        let targets = self.editor.layers.targets();
        let before = self.editor.layers.shape(self.editor.doc.layer_bytes());
        if self.editor.layers.group(&targets).is_none() {
            log::warn!("nothing to group, or the stack is full");
            return;
        }
        self.editor
            .history
            .record(Edit::new(EditKind::Group, before));
        self.editor.mark_modified();
    }

    /// Give the selected layer a mask, filled opaque white so nothing about the
    /// picture changes until something is painted into it.
    fn add_mask(&mut self) {
        // The float's preview slice is taken from the same pool, so a mask
        // allocated under one would collide with it — exactly the reason
        // `add_layer` puts the picture down first.
        self.finish_transform();
        let index = self.editor.layers.active_index();
        if self.editor.layers.locked_at(index) {
            return;
        }
        // The mask this layer has *now* — none — so restoring this shape takes
        // the new one off again and parks its slice in the entry that would put
        // it back.
        let before = self
            .editor
            .layers
            .shape_with_mask(index, self.editor.doc.layer_bytes());
        let slot = match self.editor.layers.add_mask(index) {
            Some(slot) => slot,
            // Retried after a release, never before one, and only where a slice
            // is the plausible reason — the other refusals here are "this layer
            // already has a mask" and an index off the end, neither of which a
            // released slice would mend and both of which would otherwise cost
            // the artist their oldest edits for nothing. `mask_at` answers
            // `None` to both, so the index is checked separately.
            // The shape is not re-taken: a release never touches the stack.
            None if index < self.editor.layers.len()
                && self.editor.layers.mask_at(index).is_none()
                && self.free_a_slot() =>
            {
                let Some(slot) = self.editor.layers.add_mask(index) else {
                    return;
                };
                slot
            }
            None => return,
        };
        let needed = self.editor.layers.slot_capacity_needed();
        self.editor
            .history
            .record(Edit::new(EditKind::AddMask, before));
        self.editor.mark_modified();
        // Painting the mask is what the painter almost certainly wants next,
        // and the switch is one click away either way.
        self.editor.edit_target = umber_core::EditTarget::Mask;

        let id = self.editor.session.active_id();
        let Some(gfx) = self.gfx.as_mut() else { return };
        let Some(canvas) = gfx.canvases.get_mut(&id) else {
            return;
        };
        canvas.ensure_slots(&gfx.gpu.device, &gfx.gpu.queue, needed);
        let mut enc = gfx
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("init-mask"),
            });
        // White, not cleared: a recycled slice holds the last layer's pixels,
        // and an empty mask would hide the layer outright.
        canvas.fill_layer_white(&mut enc, slot);
        gfx.gpu.queue.submit(Some(enc.finish()));
    }

    /// Take the selected layer's mask off.
    ///
    /// The one structural edit that changes the picture — what the mask hid
    /// comes back — which is why it is `EditKind::RemoveMask` and not filed
    /// under deleting a layer.
    fn remove_mask(&mut self) {
        self.finish_transform();
        self.finish_stroke();
        let index = self.editor.layers.active_index();
        if self.editor.layers.locked_at(index) {
            return;
        }
        // The claim is *cloned* into the shape before the layer's own copy is
        // taken away, so the slice stays alive with the entry holding it. That
        // is what stopped this clearing the whole history: dropping the claim
        // here would put the number straight back on the free list, and a patch
        // recorded against it would be replayed into whatever inherited it.
        let before = self
            .editor
            .layers
            .shape_with_mask(index, self.editor.doc.layer_bytes());
        if self.editor.layers.remove_mask(index).is_none() {
            return;
        }
        self.editor.edit_target = umber_core::EditTarget::Layer;
        self.editor
            .history
            .record(Edit::new(EditKind::RemoveMask, before));
        self.editor.mark_modified();
    }

    fn handle_keys(&mut self, key: KeyCode, pressed: bool) -> bool {
        // Space, Escape and Enter are decided before the binding table is
        // consulted — but by the same suspension `resolve` answers to, which is
        // the whole of `shortcuts::direct`. Read that before changing anything
        // here: all three used to be claimed unconditionally, so Enter in the
        // Text module's caption field inserted a newline *and* committed the
        // floating text, and Escape in a typable rail threw away the float
        // behind it. Note "before the table", not "outside" it — Enter is
        // genuinely bindable and only shadowed while a draft or a float
        // stands; see `Direct`.
        match shortcuts::direct(key, pressed, shortcuts::suspended()) {
            // A held modifier with press *and* release meaning, which a
            // press-resolved table cannot express.
            Some(shortcuts::Direct::PanModifier) => {
                self.editor.space_down = pressed;
                return true;
            }
            // Escape abandons the outline being drawn, and then the floating
            // transform — the layer was never written to, so the move costs
            // nothing to throw away. Each is claimed only while it exists, so
            // Escape goes on reaching whatever else on the canvas wants it.
            Some(shortcuts::Direct::Abandon) => {
                if self.editor.cancel_selection_draft() {
                    return true;
                }
                if self.cancel_transform() {
                    return true;
                }
            }
            // Enter closes the outline, or puts the floating pixels down. Same
            // pair, same terms.
            Some(shortcuts::Direct::Finish) => {
                if self.editor.selection_draft.is_some() {
                    self.editor.finish_selection();
                    return true;
                }
                if self.editor.float.is_some() {
                    self.finish_transform();
                    return true;
                }
            }
            None => {}
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
            // Only the dialog. The chord asks the question; it does not write a
            // file behind the artist's back.
            Action::Export => self.editor.export_form.open = true,
            Action::Undo => self.undo(),
            Action::Redo => self.redo(),
            Action::Deselect => self.editor.deselect(),
            Action::Copy => self.copy_selection(),
            Action::Cut => self.cut_selection(),
            Action::Paste => self.paste(),
            Action::FlipCanvasHorizontal => self.flip_canvas(umber_core::FlipAxis::Horizontal),
            Action::FlipCanvasVertical => self.flip_canvas(umber_core::FlipAxis::Vertical),
            Action::BrushTool => self.pick_tool(Tool::Brush),
            Action::EraserTool => self.pick_tool(Tool::Eraser),
            Action::SelectTool => self.pick_tool(Tool::Select),
            Action::TransformTool => self.pick_tool(Tool::Transform),
            Action::PanTool => self.pick_tool(Tool::Pan),
            Action::ZoomTool => self.pick_tool(Tool::Zoom),
            Action::SwapColours => self.editor.swap_colors(),
            // Every temporary brush change goes through one table, so the
            // shortcut and the Brush tweaks rail cannot disagree about what
            // "a bit more" is worth — see `tweaks`. The
            // two size arms used to spell 1.15 here; that figure is now what
            // `STEP_PX` of the drag comes to, which is where it came from.
            Action::SizeDown
            | Action::SizeUp
            | Action::OpacityDown
            | Action::OpacityUp
            | Action::HardnessDown
            | Action::HardnessUp
            | Action::SpacingDown
            | Action::SpacingUp
            | Action::RoundnessDown
            | Action::RoundnessUp
            | Action::AirbrushDown
            | Action::AirbrushUp
            | Action::AngleDown
            | Action::AngleUp
            | Action::PickupDown
            | Action::PickupUp => {
                if let Some((tweak, steps)) = crate::tweaks::of_action(action) {
                    tweak.nudge(&mut self.editor.brush, steps);
                }
            }
            Action::FitView => self.editor.fit_view(),
            Action::ActualSize => self.editor.camera.zoom = 1.0,
            Action::ZoomIn => self.editor.zoom_by(ZOOM_KEY_STEP),
            Action::ZoomOut => self.editor.zoom_by(1.0 / ZOOM_KEY_STEP),
        }
        true
    }

    /// Choose a tool, putting any floating transform down on the way out.
    ///
    /// The per-frame invariant in `render` would catch this a frame later, but
    /// a shortcut that changes tool should have finished with the pixels by the
    /// time the next event arrives.
    fn pick_tool(&mut self, tool: Tool) {
        if tool != Tool::Transform {
            self.finish_transform();
        }
        self.editor.set_tool(tool);
    }

    /// Start or stop the brush-size drag — Alt held down with nothing else.
    ///
    /// **Alt with a button is the eyedropper, and this is Alt without one.**
    /// That is the whole of how the two are told apart, and it is why this is
    /// driven from `ModifiersChanged` while the eyedropper is driven from
    /// `MouseInput`: a press cancels the gesture (see there), so the click that
    /// picks a colour is never also a resize, and the resize never eats a
    /// press. Neither can happen without the other having been decided first.
    ///
    /// Refused while anything else is going on. A stroke or a pan is a gesture
    /// the pointer is already committed to, and changing the brush half way
    /// through a stroke would not affect the dabs already stamped anyway — the
    /// stroke paints with the brush it began with.
    fn set_brush_resize(&mut self, wanted: bool) {
        let start = wanted
            && self.editor.interaction == Interaction::Idle
            && !self.editor.stroke.is_active();
        if start == self.editor.brush_resize.is_some() {
            return;
        }
        self.editor.brush_resize = start.then_some(crate::editor::BrushResize {
            origin: self.editor.cursor,
            from: self.editor.brush.size,
        });
        // The circle appearing and — more importantly — disappearing is a frame
        // nothing else would ask for.
        self.request_redraw();
    }

    /// What a press with the selection tool means: replace what is selected,
    /// add to it, take the new shape out of it, or keep only what both cover.
    ///
    /// **Shift adds, Ctrl — Command on macOS — subtracts, and the two together
    /// intersect.** Shift-to-add is universal and needs no defending. Subtract
    /// is the interesting half: Photoshop's and Krita's spelling of it is Alt,
    /// and Alt is not available here. It is already the eyedropper when a
    /// button goes down and the brush resize when one does not (see
    /// `set_brush_resize`), and the eyedropper is tested *before* the tool is
    /// consulted — so an Alt-drag with the selection tool in hand picks a
    /// colour today, and making it subtract instead would take a gesture away
    /// from every tool to give it to one. Ctrl is GIMP's binding for the same
    /// operation, so this is a convention somebody already has rather than one
    /// invented here, and Ctrl is otherwise unspoken for on a canvas press — it
    /// is the wheel's zoom modifier and nothing else. Intersect then follows
    /// for free: it is add-and-subtract together everywhere this is spelled,
    /// with Ctrl standing in for the Alt Umber cannot offer.
    ///
    /// **With no modifier held it is the tool options strip's setting**, which
    /// is what makes the operation reachable at all: a held key is not
    /// discoverable, cannot be listed and cannot be enumerated, so a modifier
    /// alone would leave three quarters of this feature to be guessed. It is
    /// still not in [`shortcuts`]'s rebindable table, deliberately — a held
    /// modifier is part of a *gesture*, not a command that could be fired from
    /// the keyboard — and the strip is where it is written down instead. A
    /// modifier overrides the setting for the one gesture rather than changing
    /// it, which is what every application spelling both does.
    ///
    /// Command is accepted alongside Control for the same reason
    /// `shortcuts::resolve` folds the two: winit reports it as Super, and a Mac
    /// keyboard has no Ctrl a hand reaches for.
    fn selection_op(&self) -> SelectionOp {
        combined_selection_op(
            self.modifiers.shift_key(),
            self.modifiers.control_key() || self.modifiers.super_key(),
            self.editor.ui.selection_op,
        )
    }

    /// Take the colour under the cursor as the painting colour.
    fn pick_colour_at_cursor(&mut self) {
        let doc = self.editor.screen_to_doc(self.editor.cursor);
        let size = self.editor.doc.size;
        if doc.x < 0.0 || doc.y < 0.0 || doc.x >= size.x as f32 || doc.y >= size.y as f32 {
            return;
        }

        let layers = self.editor.layer_draws(self.float_preview());
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
        // The floating pixels are not in any layer yet, so a save that ran
        // round them would write a file that disagreed with the screen. Put
        // them down first, which is what the artist meant by saving.
        self.finish_transform();
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
            // A folder holds no slice, so it reads back as nothing at all and
            // `SaveLayer::folder` writes it as a nested `<stack>` with no
            // `src`. Kept in step with the stack positionally rather than
            // filtered out, because `doc.active` and the history's positions
            // both count every entry.
            let pixels: Vec<Vec<u8>> = stack
                .iter()
                .map(|layer| match layer.slot() {
                    Some(slot) => {
                        canvas.read_layer_rect(&gfx.gpu.device, &gfx.gpu.queue, slot, rect)
                    }
                    None => Vec::new(),
                })
                .collect();
            // The masks, read the same way and only where there is one. A
            // document with no masks pays for nothing here.
            let masks: Vec<Option<Vec<u8>>> = stack
                .iter()
                .map(|layer| {
                    layer.mask().map(|slot| {
                        canvas.read_layer_rect(&gfx.gpu.device, &gfx.gpu.queue, slot, rect)
                    })
                })
                .collect();
            // The flattened preview the format requires comes from the same
            // composite pass the screen uses, so it cannot disagree with it.
            let merged = canvas.export_rgba(
                &gfx.gpu.device,
                &gfx.gpu.queue,
                &self.editor.layer_draws(None),
            );

            let layers: Vec<SaveLayer<'_>> = stack
                .iter()
                .zip(&pixels)
                .zip(&masks)
                .map(|((layer, px), mask)| SaveLayer {
                    visible: layer.visible,
                    opacity: layer.opacity,
                    mask: mask.as_deref(),
                    // Straight off the layer, because a save reads the stack
                    // it is looking at. The autosave cannot do this — its
                    // pixels arrive over several frames, so its metadata is
                    // snapshotted when the capture begins — and the two have
                    // to write the same file. See `autosave::LayerMeta`.
                    effects: layer.effects(),
                    clipped: layer.clipped,
                    locked: layer.locked,
                    link: layer.link,
                    depth: layer.depth,
                    folder: layer.is_folder(),
                    ..SaveLayer::new(&layer.name, layer.blend, px)
                })
                .collect();

            // The undo history, resolved against the stack it belongs to. No
            // GPU work: the patches have been in memory since they were
            // captured at commit time, so this adds nothing to the blocking
            // readbacks above. `SaveHistory::new` refuses outright if any patch
            // names a slot no layer holds, which cannot happen from here —
            // a deleted layer's slice is parked in the entry that could put it
            // back, and an entry naming one is cut out of the file — and is
            // checked anyway because a patch replayed into the wrong layer is
            // far worse than no saved history at all.
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

    /// Write every open document that holds work, and say whether all of them
    /// went.
    ///
    /// Each is switched to before it is saved. `save_document` reads the live
    /// document out of the editor, and a background document's state is parked
    /// in its tab — but it is also the only honest thing to do: a file dialog
    /// asking where to put a painting the painter cannot see is asking about
    /// the wrong one. The tab in front at the end is the last one saved, or the
    /// one that could not be, which is the one worth being left looking at.
    fn save_every_document(&mut self) -> bool {
        for index in self.editor.unsaved_documents() {
            self.switch_document(index);
            if !self.save_document(false) {
                return false;
            }
        }
        true
    }

    /// Flatten the visible stack and write it out in the format the export
    /// dialog settled on.
    ///
    /// The pixels come from `export_rgba` — the screen composite pass with an
    /// export flag — and nothing here flattens anything. That is the whole
    /// reason a white-backed document exports opaque and a transparent one
    /// keeps its alpha without this function knowing a `Background` exists: the
    /// background composites under the stack *inside* that pass.
    ///
    /// Encoding and writing happen here, on the event loop, exactly as an
    /// explicit Save's blocking readback does. It is not threaded, and that is
    /// a decision rather than an omission: the file dialog immediately above
    /// blocks the application anyway, no stroke can be in flight (the
    /// transform is committed and the pointer is on a menu), and a threaded
    /// encode would have to hold a copy of the whole picture and report its
    /// failure into a document that may by then be a different one. The
    /// autosave threads its writer because *nobody asked for it*; this one was
    /// asked for.
    fn export(&mut self, options: umber_core::ExportOptions) {
        self.finish_transform();
        let id = self.editor.session.active_id();
        let suggested =
            export::default_file_name(self.editor.session.active_title(), options.format);
        let Some(gfx) = self.gfx.as_ref() else { return };
        let Some(canvas) = gfx.canvases.get(&id) else {
            return;
        };

        let Some(picked) = rfd::FileDialog::new()
            .set_title(format!("Export {}", options.format.label()))
            .add_filter(options.format.filter(), options.format.extensions())
            .set_file_name(suggested)
            .save_file()
        else {
            return;
        };
        // The format is the dialog's, so a name that disagrees with it is
        // reported rather than obeyed — a filename must not overrule a control
        // the artist just set. See `export::target`.
        let target = export::target(&picked, options.format);
        let name = file_name_of(&target.path);

        let layers = self.editor.layer_draws(None);
        let pixels = canvas.export_rgba(&gfx.gpu.device, &gfx.gpu.queue, &layers);
        let size = self.editor.doc.size;

        let written = export::encode(&pixels, size.x, size.y, &options)
            .map_err(|e| e.to_string())
            .and_then(|bytes| {
                // `docformat`'s atomic write, not a second temp-and-rename: an
                // export that dies halfway must not replace a good file with a
                // truncated one either.
                docformat::write_encoded(&target.path, &bytes)
                    .map(|()| bytes.len())
                    .map_err(|e| e.to_string())
            });

        match written {
            Ok(bytes) => {
                log::info!(
                    "exported {} — {} × {} as {}, {bytes} bytes",
                    target.path.display(),
                    size.x,
                    size.y,
                    options.format.label(),
                );
                // Said out loud, because it is the one thing about this export
                // the artist did not choose. Silence would leave them looking
                // for a file under the name they typed.
                if let Some(named) = target.named {
                    self.editor.notice = Some(Notice {
                        title: format!("Exported as “{name}”"),
                        lines: vec![format!(
                            "The name given ended in .{} but {} was the format chosen, so \
                             {}'s own extension was added.",
                            named.extension(),
                            options.format.label(),
                            options.format.label(),
                        )],
                    });
                }
            }
            Err(error) => {
                log::error!("could not export {}: {error}", target.path.display());
                self.editor.notice = Some(Notice {
                    title: format!("Could not export “{name}”"),
                    lines: vec![error],
                });
            }
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
        self.editor.updates.poll(std::time::Instant::now());

        // What the panic hook is allowed to say about the artist's documents.
        // The hook cannot borrow the editor — it has no reference to one, and
        // the frame that panicked may be halfway through changing it — so the
        // answer is kept in a snapshot beside it. Sitting on the drawing path
        // is only defensible because this reduces the tab strip to one number
        // first and returns without allocating while that number is unchanged,
        // which is every frame but the handful where something opened, closed,
        // was saved or was painted on. See `crash::note_documents`.
        crash::note_documents(&self.editor.session);

        // Also before the interface is built, and that is the whole point: a
        // slot is a slice of *one* document's texture array, so the frame after
        // a tab switch would otherwise draw the new document's rows wearing the
        // old one's pictures for the same slot numbers. `thumbs::request` runs
        // after the UI — it needs the frame's encoder — which is too late to be
        // the only place this is asked.
        self.editor.thumbs.follow(self.editor.session.active_id());

        let Some(gfx) = self.gfx.as_mut() else { return };

        let now = std::time::Instant::now();
        if let Some(prev) = self.last_frame {
            self.editor.record_frame_time((now - prev).as_secs_f32());
        }
        self.last_frame = Some(now);

        // Restyling walks the whole style struct, so only do it when the theme
        // actually changed rather than every frame.
        let wanted = self.editor.palette();
        if self.applied_theme != Some(wanted) {
            theme::apply(&gfx.egui_ctx, &wanted);
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
        // Read before the output is handed over, because it is moved. This is
        // *what the interface asked for this frame* — `ui::pen_cursor` is the
        // only thing that ever asks for `None` — so the state runs the same way
        // it does for every other cursor: derived per frame, never remembered.
        //
        // Focus is deliberately *not* re-tested here. It is folded into the
        // request, in `Editor::pen_dot`, and testing it a second time on this
        // side is what let the two disagree: the request said "none" while the
        // platform call was skipped, so nothing ever put the arrow back. One
        // condition, and the platform call happens exactly when the frame asked
        // for no cursor.
        let hide_cursor = platform_output.cursor_icon == egui::CursorIcon::None;
        gfx.egui_state
            .handle_platform_output(&gfx.window, platform_output);
        // …and after, because egui-winit's own attempt goes first and, on
        // Windows under a pen, quietly does nothing. See `syscursor`: winit
        // hides the cursor only while a flag that legacy mouse messages alone
        // ever set is on, and a pen produces none of those. Called every frame
        // the answer is still "none" rather than on the change, so there is
        // nothing here to get stuck.
        if hide_cursor {
            syscursor::hide_now();
        }
        // Observation only, and the answer this frame acted on rather than a
        // second reading of it — the rule every column of Settings → Input &
        // pen lives by. It is the one place "Umber never asked" and "Umber
        // asked and the platform ignored it" can be told apart, on a machine
        // that actually has a tablet.
        //
        // `obscured` is what makes the row worth reading at all. egui's
        // `layer_id_at` answers the *modal's* layer for every point in the
        // window while one is open — not a hit test, see `over_egui_area` — so
        // `pen_dot` correctly declines everywhere, and every frame the user can
        // actually see this pane on is a frame that says nothing about the pen.
        // Recording those would pin the row to one answer for ever.
        let obscured = gfx.egui_ctx.memory(|m| m.top_modal_layer().is_some());
        self.editor.input.note_cursor(hide_cursor, obscured);

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

        // A reconfigure an earlier frame asked for and could not do itself,
        // paid here — before anything is acquired, which is the whole point of
        // deferring it. See `swapchain`.
        if gfx.reconfigure_pending {
            gfx.reconfigure_surface();
        }

        // What wgpu answered, reduced to something `swapchain::plan` can
        // decide on. Nothing is decided in these arms: which of them keeps its
        // texture and which of them reconfigures — and, the part that was a
        // crash, *when* — is the model's, so it can be tested without a
        // surface. The harness has none.
        let (acquisition, texture) = match gfx.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => (swapchain::Acquisition::Fresh, Some(t)),
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => {
                (swapchain::Acquisition::Suboptimal, Some(t))
            }
            wgpu::CurrentSurfaceTexture::Outdated => (swapchain::Acquisition::Outdated, None),
            wgpu::CurrentSurfaceTexture::Lost => (swapchain::Acquisition::Lost, None),
            wgpu::CurrentSurfaceTexture::Occluded => (swapchain::Acquisition::Occluded, None),
            wgpu::CurrentSurfaceTexture::Timeout => (swapchain::Acquisition::Timeout, None),
            other => {
                log::warn!("could not acquire surface texture: {other:?}");
                (swapchain::Acquisition::Failed, None)
            }
        };
        // The one line where wgpu's answer and the model's meet, and therefore
        // the one place they can disagree. Everything `swapchain` proves is
        // about answers it was *shown*: map a texture-carrying acquisition
        // onto `Outdated` here and the Wayland crash comes back with every
        // test in that module still passing, because the plan would not be
        // wrong — this translation would. Debug-only: it is a statement about
        // the two arms above, which cannot change at runtime.
        debug_assert_eq!(
            acquisition.carries_texture(),
            texture.is_some(),
            "{acquisition:?} was translated from an answer that disagrees with it"
        );
        let frame = swapchain::plan(acquisition);
        // A texture this frame is not going to draw into is let go of *before*
        // the surface is touched, and this is the order rather than the
        // reverse: a reconfigure is refused while any acquired texture is
        // alive, whether or not the frame holding it meant to use it.
        let acquired = texture.filter(|_| frame.draws());
        if frame.reconfigure_now() {
            gfx.reconfigure_surface();
        }
        // Where a texture *is* being drawn into, the reconfigure waits for the
        // frame after this one. Doing it here — which is what this code used
        // to do on `Suboptimal` — is the validation error "`SurfaceOutput`
        // must be dropped before a new `Surface` is made", and
        // `crash::device_error` makes that fatal, as it must.
        //
        // No redraw is asked for to carry it out, deliberately. This frame was
        // drawn and is correct — a suboptimal swapchain is the wrong size or
        // scale for the window, not the wrong picture — and asking for a frame
        // whose only purpose is to reconfigure is the shape that burned a
        // fifth of a core before `repaint_at` existed, since a driver that
        // keeps answering `Suboptimal` would keep asking. On the desktop what
        // makes a surface suboptimal is a resize or a scale change, and winit
        // emits `Resized` for both — including on Wayland, where a scale
        // change queues one — which requests a redraw and reconfigures on the
        // spot. Vulkan also reports it for a surface *transform* change, which
        // is a device rotation: no desktop produces one, and it is the case to
        // re-examine if Android is ever built.
        gfx.reconfigure_pending |= frame.reconfigure_later();

        // The renderer of the document in front. Every other open document has
        // one of its own, holding its pixels, untouched until it is switched to.
        let has_canvas = gfx.canvases.contains_key(&self.editor.session.active_id());
        if !has_canvas {
            log::error!("the active document has no canvas renderer");
        }

        let Some(surface_texture) = acquired.filter(|_| has_canvas) else {
            // Frees are applied even though nothing was drawn, and here — and
            // only here — they may be applied at once: no command buffer was
            // recorded, so nothing can be holding a draw against them. See
            // `submit_frame` for why the ordering matters everywhere else.
            release_finished_textures(&mut gfx.egui_renderer, &textures_delta.free);
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
        // A float counts as busy even with the pointer up. Its pixels are not
        // in any layer yet, so a document autosaved mid-transform would be
        // written without them — and the file would then disagree with the
        // screen, which is the one thing an autosave must not do.
        let quiet = self.editor.interaction == Interaction::Idle
            && !self.editor.stroke.is_active()
            && self.editor.float.is_none()
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
        // The stroke's own snapshot — see `finish_stroke`, which builds the
        // same style from the same field so the two frames of one stroke cannot
        // be drawn by two different pipelines.
        let dab_style = DabStyle {
            per_dab_color: self.editor.stroke_style.per_dab_color,
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

        // Before the composite, into the same encoder: the preview slice has to
        // hold this frame's position of the picture by the time the stack is
        // drawn. It restores only what the previous frame wrote plus what this
        // one will, and allocates nothing.
        if let Some(float) = self.editor.float {
            canvas.draw_float(
                &gfx.gpu.queue,
                &mut encoder,
                &FloatParams {
                    inverse: float.xf.inverse(),
                    dest: float.xf.dest_rect(self.editor.doc.size),
                },
            );
        }

        let layer_draws = self.editor.layer_draws(canvas.float_preview());

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
                    active_index: self.editor.active_draw_index(),
                    stroke: self.editor.stroke_style,
                    doc_point: point,
                    radius,
                },
            );
        }
        // One layer thumbnail's next pass, into the same encoder. Requested and
        // driven here rather than from the panel because the panel has no
        // encoder and no device; what the panel decides is only *which* slot,
        // and that is `Thumbs::wanted`'s, in a model with no drawing in it.
        //
        // At most one job is in flight at a time, so a document with sixty
        // layers fills its list over a couple of seconds rather than in one
        // frame — which is the right trade for something nobody is waiting on.
        thumbs::request(&mut self.editor, canvas);
        canvas.drive_thumb(&gfx.gpu.device, &mut encoder);

        canvas.composite(
            &gfx.gpu.queue,
            &mut encoder,
            &view,
            &CompositeParams {
                camera: &self.editor.camera,
                pivot: self.editor.canvas_pivot,
                layers: &layer_draws,
                active_index: self.editor.active_draw_index(),
                stroke: self.editor.stroke_style,
                backdrop: self.editor.palette().backdrop_display(),
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
        // Submit, and only then give egui's finished textures back. The two are
        // one call because they must not be separable — see `submit_frame`.
        submit_frame(
            &gfx.gpu,
            &mut gfx.egui_renderer,
            encoder,
            &textures_delta.free,
        );
        surface_texture.present();

        // The probe's copy is only submitted now, so mapping it has to wait
        // until here. Collecting is a non-blocking poll: whatever came home
        // feeds the stroke, and whatever did not is picked up next frame.
        // Gated on the *brush*, not on `dab_style`. The probe exists to tell a
        // smudging stroke what it is passing over, and a coloured stamp is on
        // the same colour path without wanting any of that — sampling the canvas
        // for a stroke that never reads the answer is a readback a frame, in
        // rotation, for nothing.
        if self.editor.stroke.is_coloured()
            && let Some(canvas) = gfx.canvases.get_mut(&self.editor.session.active_id())
        {
            canvas.submit_probes();
            if let Some(sample) = canvas.take_probe(&gfx.gpu.device) {
                self.editor.stroke.absorb(sample);
            }
        }

        // The thumbnail's copy, mapped now that the frame holding it has been
        // submitted, and collected by a poll that never waits — the smudge
        // probe's arrangement exactly, and for the same reason.
        if let Some(canvas) = gfx.canvases.get_mut(&self.editor.session.active_id()) {
            canvas.submit_thumb();
            if let Some(thumb) = canvas.take_thumb(&gfx.gpu.device) {
                self.editor.thumbs.accept(&gfx.egui_ctx, thumb);
                // The list is not otherwise redrawn until something happens,
                // and a thumbnail arriving is something happening.
                gfx.window.request_redraw();
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

        // An offer raised by the collect above arrived *after* this frame was
        // presented, so nothing on screen holds it — and under
        // `ControlFlow::Wait` a value appearing in a field is not an event.
        // The same wake-up the update check and the autosave's writer need.
        if self.editor.recovery.take_arrived() {
            gfx.window.request_redraw();
        }

        // Keep the frames coming while a stroke is live; otherwise the app
        // goes back to sleep until the next input event. A capture in flight
        // needs the same: under `ControlFlow::Wait` a document being read back
        // would otherwise stop dead the moment the painter took their hand off
        // the mouse, which is exactly when it started.
        // A thumbnail in flight needs the same: it takes several frames, and
        // under `ControlFlow::Wait` a list left half filled in would stay that
        // way until the user moved the mouse.
        if self.editor.interaction == Interaction::Drawing
            || self.editor.autosave.capturing()
            || gfx
                .canvases
                .get(&self.editor.session.active_id())
                .is_some_and(CanvasRenderer::thumb_in_flight)
        {
            gfx.window.request_redraw();
        }

        // Applied after the `gfx` borrow ends, since these take `&mut self`.

        // A float only ever exists with the transform tool in hand and on the
        // layer it was picked up from. Checked here, once, rather than at every
        // control that can change either: the rail, the Window menu's
        // shortcuts, the layer list and a preset all reach one of them, and an
        // invariant enforced at five call sites is one that will be forgotten
        // at the sixth. The preview would otherwise go on standing in front of
        // a layer nobody is editing.
        if let Some(float) = self.editor.float
            && (self.editor.ui.tool != Tool::Transform
                || self.editor.layers.active_slot() != Some(float.slot))
        {
            self.finish_transform();
        }

        if actions.undo {
            self.undo();
        }
        if actions.redo {
            self.redo();
        }
        if let Some(axis) = actions.flip_canvas {
            self.flip_canvas(axis);
        }
        if let Some(position) = actions.history_jump {
            self.jump_history(position);
        }
        if actions.clear {
            self.clear_active_layer();
        }
        // The selection's own strip of controls. The GPU is what a copy and a
        // cut need, so like every other entry here they come back as a request
        // rather than being carried out where they were drawn.
        if actions.copy_selection {
            self.copy_selection();
        }
        if actions.cut_selection {
            self.cut_selection();
        }
        if actions.paste {
            self.paste();
        }
        if actions.open_export {
            self.editor.export_form.open = true;
        }
        if let Some(options) = actions.export {
            self.export(options);
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
        if let Some(index) = actions.use_tip_and_close {
            // Paired for the reason `save_and_close` is: the prompt can be
            // raised on a tab that is not in front, and taking one canvas as a
            // stamp while closing another would lose the work in the most
            // confusing way available.
            self.switch_document(index);
            // Closed only if the stamp was actually taken. A canvas nobody has
            // painted on is refused, and the tab stays open holding it.
            if self.commit_tip() {
                self.close_document(index);
            }
        }
        if actions.place_text {
            self.place_text();
        }
        if actions.group_layers {
            self.group_layers();
        }
        if actions.add_layer {
            self.add_layer();
        }
        if actions.add_mask {
            self.add_mask();
        }
        if actions.remove_mask {
            self.remove_mask();
        }
        if let Some(index) = actions.delete_layer {
            self.delete_layer(index);
        }
        if actions.delete_picked {
            self.delete_picked_layers();
        }
        if let Some(index) = actions.move_layer_up {
            self.record_move(|layers| layers.move_up(index).is_some());
        }
        if let Some(index) = actions.move_layer_down {
            self.record_move(|layers| layers.move_down(index).is_some());
        }
        if actions.fit_view {
            self.editor.fit_view();
        }
        if actions.reset_zoom {
            self.editor.camera.zoom = 1.0;
        }
        if actions.zoom_in {
            self.editor.zoom_by(ZOOM_KEY_STEP);
        }
        if actions.zoom_out {
            self.editor.zoom_by(1.0 / ZOOM_KEY_STEP);
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
        if actions.new_tip {
            self.new_tip_document();
        }
        if actions.commit_tip {
            self.commit_tip();
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
        // Before the dismissal, so a frame in which both were somehow set still
        // opens what was asked for before the offer is put away.
        if actions.recover {
            self.recover_documents();
        }
        if actions.dismiss_recovery {
            self.dismiss_recovery();
        }
        if actions.save_all_and_quit {
            self.editor.ui.quit_prompt = false;
            // Quits only if every document was actually written. A cancelled
            // file dialog on the third of four is not permission to discard the
            // other three — the same reading of "Save" the close prompt takes.
            self.editor.quit_requested = self.save_every_document();
            if !self.editor.quit_requested {
                self.editor.ui.quit_prompt = true;
            }
        }
        if actions.quit {
            self.editor.ui.quit_prompt = false;
            self.editor.quit_requested = true;
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
        self.finish_transform();
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
        self.finish_transform();
        self.finish_stroke();
        let id = self.editor.create_document(doc);
        let slots = self.editor.layers.slot_capacity_needed();
        if let Some(gfx) = self.gfx.as_mut() {
            gfx.add_canvas(id, &doc, slots);
        }
        self.request_redraw();
    }

    /// Open a canvas to draw the brush in hand's bitmap tip on.
    ///
    /// What the canvas *is* — square, 256 pixels, transparent — is
    /// `umber_core::tip::authoring_document`'s, with the argument for each half
    /// beside it. Nothing about the shape of it is decided here.
    ///
    /// An ordinary document in every other respect: it has an undo history, it
    /// can be saved as a picture, and closing it unsaved asks the same question
    /// every other tab asks. Only the tab's [`Tab::tip_for`] says otherwise,
    /// which is what the strip along the top and "Use as tip" read.
    ///
    /// [`Tab::tip_for`]: crate::session::Tab::tip_for
    fn new_tip_document(&mut self) {
        let Some(preset) = self
            .editor
            .active_preset
            .and_then(|i| self.editor.presets.get(i))
        else {
            return;
        };
        let target = umber_core::TipTarget::new(&preset.id, &preset.name);
        let doc = umber_core::tip::authoring_document();

        self.finish_transform();
        self.finish_stroke();
        let id =
            self.editor
                .open_document(DocumentState::blank(doc), target.title(), None, Vec::new());
        let tab = self.editor.session.active_tab_mut();
        tab.tip_for = Some(target);
        let slots = self.editor.layers.slot_capacity_needed();
        if let Some(gfx) = self.gfx.as_mut() {
            gfx.add_canvas(id, &doc, slots);
        }
        // The brush editor is a modal over the canvas the artist has just been
        // sent to; leaving it up would hide the thing they are meant to paint.
        self.editor.ui.brush_editor_open = false;
        self.request_redraw();
    }

    /// Turn the tip canvas in front into the stamp it was opened for.
    ///
    /// The pixels come off `export_rgba` — the *screen* composite pass with an
    /// export flag, which is the same one the PNG export, the eyedropper and
    /// the autosave use, so what is stamped is byte for byte what the artist
    /// was looking at. There is deliberately no second flattener here.
    ///
    /// It answers with straight-alpha sRGB, and the alpha is the whole of what
    /// is taken: `TipMask::from_alpha` has the argument, and the short version
    /// is that a tip document starts transparent, so its alpha *is* the paint
    /// laid on it. Colour is discarded because a tip has none — the palette
    /// decides that at painting time.
    ///
    /// Where the mask *goes* is `brushlib::commit_tip`'s, including both ways
    /// it can fail to reach the brush it was drawn for.
    /// Answers whether the canvas was taken. `false` means the stamp is still
    /// only on the canvas — nothing was painted, the mask was refused, or the
    /// document has no renderer — which is what stops the close prompt closing
    /// a tab that still holds the only copy of the work.
    fn commit_tip(&mut self) -> bool {
        self.finish_transform();
        self.finish_stroke();
        let Some(target) = self.editor.session.active_tab().tip_for.clone() else {
            return false;
        };
        let id = self.editor.session.active_id();
        // Scoped so the borrow of `self.gfx` ends before the editor is taken
        // mutably below. The context is an `Arc` inside, so cloning it is a
        // refcount rather than a copy of egui's state.
        let Some((ctx, pixels)) = ({
            let gfx = self.gfx.as_ref();
            gfx.and_then(|gfx| gfx.canvases.get(&id).map(|canvas| (gfx, canvas)))
                .map(|(gfx, canvas)| {
                    let layers = self.editor.layer_draws(None);
                    (
                        gfx.egui_ctx.clone(),
                        canvas.export_rgba(&gfx.gpu.device, &gfx.gpu.queue, &layers),
                    )
                })
        }) else {
            return false;
        };

        let size = self.editor.doc.size;
        let mask = match umber_core::TipMask::from_alpha(size.x, size.y, &pixels) {
            Ok(mask) => mask,
            Err(error) => {
                self.editor.notice = Some(Notice {
                    title: "That canvas cannot be a brush tip".to_string(),
                    lines: vec![error.to_string()],
                });
                return false;
            }
        };
        // A canvas nobody has painted on is not a brush. Caught here rather
        // than at the library, because the honest answer is "there is nothing
        // on it yet" rather than a file error — and because a mask of all
        // zeroes would be a brush that silently paints nothing.
        if mask.coverage().iter().all(|&coverage| coverage == 0) {
            self.editor.notice = Some(Notice {
                title: "There is nothing on this canvas yet".to_string(),
                lines: vec![
                    "Paint the stamp first. What you paint becomes coverage: colour is \
                     ignored and opacity is the strength."
                        .to_string(),
                ],
            });
            return false;
        }

        let outcome = crate::brushlib::commit_tip(&ctx, &mut self.editor, &target, mask);
        // Raised as a dialog rather than left in the brush library's own notice
        // strip, because the Brushes panel can be closed — and this one writes
        // to the user's library, so it has to be seen whatever the layout is
        // doing. It is one dialog per deliberate press, not a recurring one.
        self.editor.notice = Some(Notice {
            title: outcome.title,
            lines: vec![outcome.detail],
        });
        self.request_redraw();
        true
    }

    /// Apply the Canvas settings dialog's answer to the document in front.
    ///
    /// The stroke is finished first: a resize throws the scratch surface away,
    /// so a stroke still in flight would be lost rather than committed. Then
    /// the editor takes the new document — which is also what clears the undo
    /// history when the geometry moves — and the GPU carries the pixels across.
    fn apply_canvas(&mut self, change: canvasdlg::CanvasChange) {
        // Before the stroke, because a resize throws the float's storage away
        // too — and its rectangles name pixels of a canvas that is about to
        // stop existing.
        self.finish_transform();
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
        self.finish_transform();
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
        self.open_import(path, name, Some(path.to_path_buf()), false);
    }

    /// Open `source` as a document, presenting it as `title` and remembering
    /// `record_path` as the file it belongs to.
    ///
    /// Those two are the same file for everything but a **recovered autosave
    /// copy**, and there they must not be: the pixels come out of the internal
    /// copy while the tab has to point at the file the painter chose, or Save
    /// would write into Umber's own autosave folder — somewhere they would
    /// never think to look for their painting, and somewhere the expiry sweep
    /// is the only thing that reads.
    ///
    /// `modified` puts the dot on the tab, which a recovery needs and an
    /// ordinary open does not: the pixels on screen are not what is at
    /// `record_path`, so closing without saving would lose the difference.
    ///
    /// Returns whether a document was actually opened.
    fn open_import(
        &mut self,
        source: &Path,
        name: String,
        record_path: Option<PathBuf>,
        modified: bool,
    ) -> bool {
        // The document in front is about to be parked, and a float belongs to
        // it — its preview lives in *that* document's renderer, which would go
        // on standing in front of a layer when the tab came back.
        self.finish_transform();
        self.finish_stroke();

        let imported = match umber_core::docimport::import(source) {
            Ok(doc) => doc,
            Err(error) => {
                // `ImportError` displays as a finished sentence written for the
                // user; showing it verbatim beats inventing a second wording.
                log::warn!("could not open {}: {error}", source.display());
                self.editor.notice = Some(Notice {
                    title: format!("Could not open “{name}”"),
                    lines: vec![error.to_string()],
                });
                return false;
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
                return false;
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
                // Nothing in any format Umber reads carries one, and a
                // selection invented at import would be a claim about the
                // artist's intent that the file did not make.
                selection: None,
                // A document just opened is a document being looked at, not one
                // whose masks are being edited — whatever the last document was.
                edit_target: umber_core::EditTarget::Layer,
            },
            name.clone(),
            record_path,
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
            source.display(),
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
        // A recovered copy is not what is at the path the tab now names, so the
        // dot goes on and closing the tab asks — the same reading of `modified`
        // every other path here takes. After `open_document`, because that is
        // what made this tab the active one.
        if modified {
            self.editor.mark_modified();
            // And that the file this tab names is one nobody has saved to. The
            // autosave writes the internal copy either way and leaves the
            // painter's own file alone until they choose it — see
            // `session::Tab::recovered`.
            self.editor.session.mark_recovered();
        }
        self.request_redraw();
        true
    }

    /// Open the autosave copies the recovery offer was asked for.
    ///
    /// Each becomes an ordinary document wearing the identity it had before the
    /// crash — its own title, and its own file if it had one — so a Save writes
    /// where the painter would expect and never into Umber's autosave folder.
    /// **The copy itself is left exactly where it is.** Recovering is a read;
    /// only `autosave::Reaper` ever removes one, on its own schedule.
    fn recover_documents(&mut self) {
        for (row, entry) in self.editor.recovery.take_wanted() {
            log::info!("recovering “{}” from {}", entry.title, entry.copy.display());
            // The row is marked only on success. A copy that will not open —
            // a truncated archive, a canvas this GPU cannot hold — raises a
            // notice of its own, and the button that would let somebody try
            // again has to still be under it rather than replaced by the word
            // "Opened" beside a document that is not there.
            if self.open_import(&entry.copy, entry.title, entry.original, true) {
                // The document it came out of *is* its copy. Without this the
                // marker would describe it as having none until its first
                // autosave, and a never-saved one would then be written to a
                // second file beside the one it was recovered from.
                let id = self.editor.session.active_id();
                self.editor.autosave.adopt_copy(id, entry.copy);
                self.editor.recovery.note_opened(row);
            }
        }
    }

    /// The recovery offer has been answered, one way or the other.
    ///
    /// Forgets the markers it came from so the same offer is not made on every
    /// start for ever. Nothing else is removed: the copies are documents, and
    /// the one thing in Umber that may delete one of those is `Reaper`.
    fn dismiss_recovery(&mut self) {
        let marks = self.editor.recovery.dismiss();
        self.editor.autosave.forget_marks(&marks);
    }

    // --- the pointer, whichever kind it is ---
    //
    // A mouse reaches winit as `CursorMoved` and `MouseInput`; a pen on Windows
    // Ink reaches it as `WindowEvent::Touch` and produces neither. The three
    // functions below are what both families call, so a gesture is decided in
    // one place and a tablet cannot be left behind by a change made for a
    // mouse. What each family still owns is only what it alone can know: which
    // button, which contact id, and what the device reported for pressure.

    /// A press landed on the canvas. Returns what it was taken to mean, which
    /// is what the touch path uses to decide whether this contact owns the
    /// gesture that follows.
    ///
    /// `reported` is the device's pressure where there is one — a mouse has
    /// none and passes `None`.
    fn pointer_pressed(
        &mut self,
        pos: Vec2,
        reported: Option<f32>,
        pointer: gesture::Pointer,
    ) -> gesture::Press {
        let decision = gesture::press(pointer);
        // Observation only, and the *resolved* answer rather than a second run
        // of the decision — the same rule `note_resolved` lives by. See
        // `inputlog`.
        self.editor.input.note_gesture(decision);

        // A press that begins something *else* ends the stroke that is running,
        // and it has to happen here rather than at the call sites: a pan takes
        // `Interaction` over, and `pointer_released` dispatches on
        // `Interaction`, so the button that was drawing comes up and never
        // reaches `finish_stroke` at all. Finish rather than cancel, and not
        // what `Contact::Pinch` does — `gesture::supersedes_stroke` has the
        // whole argument and both failure modes.
        if gesture::supersedes_stroke(decision) && self.editor.stroke.is_active() {
            self.finish_stroke();
        }

        // Every press ends the brush-size drag except the one that is carrying
        // it on. A mouse press is never a contact, so `press` can never answer
        // `ResizeBrush` for one and this stays exactly the rule it always was:
        // Alt with a button is the eyedropper, Alt without one is the resize,
        // and a press is what tells them apart.
        if decision != gesture::Press::ResizeBrush {
            self.set_brush_resize(false);
        }
        if decision == gesture::Press::Ignored {
            return decision;
        }
        // A touch carries its own position and `Editor::cursor` does not follow
        // it, so this is where a pen's press puts it. For a mouse it is what
        // `CursorMoved` already left there.
        self.editor.cursor = pos;
        self.editor.last_cursor = pos;

        match decision {
            gesture::Press::Ignored | gesture::Press::ResizeBrush => {}
            gesture::Press::Pan => self.editor.interaction = Interaction::Panning,
            gesture::Press::Zoom => {
                self.editor.zoom_anchor = pos;
                self.editor.interaction = Interaction::Zooming;
            }
            gesture::Press::Paint => {
                let point = self.editor.sample(pos, reported);
                self.start_stroke(point);
            }
            gesture::Press::Select => {
                let doc = self.editor.screen_to_doc(pos);
                // The same op the mouse path takes. A pen user holding Shift
                // means to add to the selection just as a mouse user does.
                let op = self.selection_op();
                self.editor.selection_press(doc, op);
            }
            gesture::Press::Transform => self.transform_press(pos),
            gesture::Press::Eyedropper => self.pick_colour_at_cursor(),
        }
        decision
    }

    /// The pointer moved. Returns whether the frame is worth asking for.
    fn pointer_moved(&mut self, pos: Vec2, reported: Option<f32>) -> bool {
        self.editor.last_cursor = self.editor.cursor;
        self.editor.cursor = pos;
        let pivot = self.editor.canvas_pivot;
        let mut dragging_float = false;

        match self.editor.interaction {
            Interaction::Drawing => {
                let point = self.editor.sample(pos, reported);
                self.editor.stroke.extend(point);
            }
            Interaction::Selecting => {
                let doc = self.editor.screen_to_doc(pos);
                self.editor.selection_moved(doc);
            }
            Interaction::Panning => {
                let delta = pos - self.editor.last_cursor;
                self.editor.camera.pan_by_screen(delta);
            }
            Interaction::Zooming => {
                // Zooms about where the drag started, which is the convention
                // every other paint app uses. Right and up zoom in; how the two
                // axes combine into one factor is `Camera::zoom_drag_factor`'s,
                // so it can be reasoned about and tested without a pointer.
                let delta = pos - self.editor.last_cursor;
                let factor = umber_core::Camera::zoom_drag_factor(delta);
                let anchor = self.editor.zoom_anchor;
                self.editor.camera.zoom_at(anchor, factor, pivot);
            }
            // Nothing is held, so two things live here, and they are mutually
            // exclusive by construction: the Alt-held resize needs Alt down and
            // no button, and a polygon draft only exists once a click has landed
            // a vertex.
            //
            // The resize reads the pointer's travel from where Alt went down as
            // the size, absolutely rather than stepped per event. Both axes,
            // right and up bigger — the directions the zoom drag uses for
            // "more", resolved onto one distance by the same
            // `geom::drag_towards_more`.
            //
            // The draft case is a polygon whose gesture was interrupted, by a
            // middle drag to pan say, which takes the interaction over without
            // abandoning the outline. The rubber band still has to follow the
            // pointer, or the tool looks dead until the next click lands a
            // vertex somewhere unexpected.
            Interaction::Idle => {
                if let Some(resize) = self.editor.brush_resize {
                    self.editor.brush.size =
                        Brush::size_after_drag(resize.from, pos - resize.origin);
                } else if self.editor.selection_draft.is_some() {
                    let doc = self.editor.screen_to_doc(pos);
                    self.editor.selection_moved(doc);
                    self.editor.interaction = Interaction::Selecting;
                } else {
                    // A transform handle, which is a third thing that lives in
                    // this arm — and is exclusive with the other two the same
                    // way, since a float only exists with the transform tool in
                    // hand and the resize needs Alt with nothing pressed. Shift
                    // constrains a corner to one scale on both axes, as it does
                    // everywhere else; a touch screen has no Shift to hold, so
                    // reading it here rather than passing `false` on the touch
                    // path costs a finger nothing and gives a pen with a
                    // keyboard beside it the constraint a mouse has.
                    dragging_float = self.transform_moved(pos, self.modifiers.shift_key());
                }
            }
        }

        self.editor.interaction != Interaction::Idle
            || dragging_float
            || self.editor.brush_resize.is_some()
    }

    /// The press was let go of. `contact` is whether it was a pen or a finger
    /// leaving the glass rather than a button coming up.
    fn pointer_released(&mut self, pos: Vec2, contact: bool) {
        self.editor.cursor = pos;

        // A contact that was carrying the brush-size drag is the one gesture a
        // pen spells differently from a mouse, and this is where the difference
        // is settled: it travelled, so it was the resize; it did not, so it was
        // the tablet's Alt-click and therefore the eyedropper. See
        // `gesture::press`.
        if contact && let Some(resize) = self.editor.brush_resize {
            if gesture::is_tap(resize.origin.distance(pos)) {
                // Put back the wobble the tap made of the size before reading
                // the colour: a click is not allowed to have resized anything.
                self.editor.brush.size = resize.from;
                self.pick_colour_at_cursor();
            }
            // Re-seat the drag where the nib left off. Alt is still held — the
            // gesture is armed until `ModifiersChanged` says otherwise — and
            // without this the next contact would measure from where Alt went
            // down and jump the size back.
            self.set_brush_resize(false);
            self.set_brush_resize(true);
            return;
        }

        // A handle let go of. This ends the drag and not the transform — unless
        // it was a click outside the box, which `Self::transform_release` is
        // what knows.
        self.transform_release();
        match self.editor.interaction {
            Interaction::Drawing => self.finish_stroke(),
            // The polygon is the one gesture a release does not end — it is a
            // sequence of clicks, and stopping on the first button-up would make
            // it a line every time. `selection_release` is what knows that, so
            // the interaction is left alone here.
            Interaction::Selecting => {
                let doc = self.editor.screen_to_doc(pos);
                self.editor.selection_release(doc);
            }
            _ => self.editor.interaction = Interaction::Idle,
        }
    }

    fn request_redraw(&self) {
        if let Some(gfx) = self.gfx.as_ref() {
            gfx.window.request_redraw();
        }
    }

    /// The event loop finished on purpose.
    ///
    /// Takes this run's autosave marker down, which is the whole of how the
    /// *next* start tells a shutdown from a stop. Called from [`crate::run`]
    /// and nowhere else: `run_app` returns only when the loop was told to exit,
    /// so there is one place where that is known rather than one beside each of
    /// the four `event_loop.exit()` calls — an invariant enforced at four call
    /// sites is one that will be forgotten at the fifth.
    ///
    /// Deliberately not a `Drop`. A panic unwinds through destructors, so a
    /// `Drop` would remove the marker in exactly the case it exists to leave
    /// behind.
    pub fn ended_cleanly(&mut self) {
        self.editor.autosave.end_run();
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
            // Per-platform, and quietly so: Windows uses it for the title bar,
            // X11 for the window list. Wayland ignores it entirely and takes
            // its icon from the `.desktop` file matching the app id, and on
            // macOS `set_window_icon` is documented as a no-op — there the icon
            // comes from the `.app` bundle's `Info.plist`. The executable
            // resource in `crates/umber-desktop/build.rs` is what gives
            // Explorer and the Start Menu an icon before the process starts.
            .with_window_icon(logo::window_icon())
            .with_inner_size(winit::dpi::LogicalSize::new(1440.0, 900.0));

        // Windows keeps a *second* icon per window, `ICON_BIG`, and that is the
        // one the taskbar and Alt-Tab draw. `with_window_icon` sets only
        // `ICON_SMALL`; winit's window class carries no icon either, so without
        // this the taskbar has nothing and Windows substitutes its generic
        // application icon. See `logo::taskbar_icon`.
        #[cfg(target_os = "windows")]
        let attrs = {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs.with_taskbar_icon(logo::taskbar_icon())
        };

        // Linux takes its icon from an installed `.desktop` file rather than
        // from the window, and finds that file by name — so a window with no
        // application id has no icon to be found, which is what Umber shipped
        // with. Wayland matches its app id against the entry's basename, which
        // is why this is the reverse-DNS `taskbar::APP_ID` and not "Umber".
        //
        // X11 matches the entry's `StartupWMClass` against the window's class
        // instead, and that file says `umber`, so the two platforms genuinely
        // want different strings here. `taskbar`'s tests pin both against the
        // packaging so a rename cannot quietly break either.
        //
        // Both traits spell it `with_name`, so both calls name their trait
        // explicitly. Written as a chain, the second one is ambiguous and does
        // not compile — and the tempting repair, dropping one `use`, would
        // silently set the same platform's name twice.
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
        let attrs = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            use winit::platform::x11::WindowAttributesExtX11;
            let attrs = WindowAttributesExtWayland::with_name(attrs, taskbar::APP_ID, "");
            WindowAttributesExtX11::with_name(attrs, "umber", "umber")
        };
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
        let mut splash = Splash::new(window.clone(), self.editor.palette());
        splash.show(splash::Stage::Adapter);

        let instance = Gpu::create_instance();
        let surface = instance
            .create_surface(window.clone())
            .expect("failed to create surface");
        let gpu = pollster::block_on(Gpu::new(instance, Some(&surface)))
            .expect("failed to initialise GPU");

        // wgpu's default for an error nobody asked it to hand back is one log
        // line and a panic from inside `wgpu_core`. Routed here it keeps the
        // log line, gains Umber's own wording, and travels down the same path
        // as every other crash — which is what turns "wgpu error: Validation
        // Error" in a terminal into a box that names the file the artist's work
        // is in. It stays fatal; see `crash::device_error`.
        gpu.device
            .on_uncaptured_error(std::sync::Arc::new(crash::device_error));

        // What the crash report says the picture was drawn on. Recorded as soon
        // as there is a device, because "no device had been created" is itself
        // one of the more useful things a report can say.
        crash::note_adapter(&gpu.adapter.get_info());

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
        // egui scales its own interface on Ctrl+= / Ctrl+- / Ctrl+0 by default.
        // Here those are the canvas's zoom and Fit to view, and interface scale
        // is a slider in Settings — so egui would silently take a second action
        // on every one of them, growing the panels while the artist zoomed.
        // Turned off at the context rather than swallowed per key: key presses
        // are read off the winit event before egui is asked, so there is no
        // point in the dispatch where the press could be withheld from it.
        egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
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
            // The surface was configured a few lines above, with the size the
            // window reported, so nothing is owed.
            reconfigure_pending: false,
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
        // How an update ends. The Windows installer cannot replace a program
        // that is running, so handing it the package is only half of the update
        // — the other half is getting out of its way. A portable or AppImage
        // copy has already been replaced, and the new build is started here on
        // the way out.
        match self.editor.updates.take_exit_request() {
            Some(update::Exit::Restart) => match update::relaunch() {
                Ok(()) => {
                    event_loop.exit();
                    return;
                }
                // A restart that could not start anything must leave the copy
                // that is running running. The new build is in place either
                // way; it simply waits for the next start.
                Err(message) => self.editor.updates.restart_failed(message),
            },
            Some(update::Exit::Quit) => {
                event_loop.exit();
                return;
            }
            None => {}
        }
        // The quit prompt's answer. It is drawn from `ui::draw`, which has no
        // `ActiveEventLoop` — so it sets a flag and this is where the loop
        // actually stops, exactly as the update's own quit request does.
        if self.editor.quit_requested {
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
        //
        // A floating transform does not survive it, and cannot: its pixels are
        // in textures that go with the renderers. Dropped rather than
        // committed, because committing needs the GPU that is being taken
        // away — the same bargain the pixels themselves have always struck on
        // this path.
        self.editor.float = None;
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
        // The mouse's answer, from the position `CursorMoved` last reported.
        // Anything carrying its own position — a touch, a pen — must ask
        // `ui_owns_pointer` about *that* instead. See there.
        let ui_has_pointer = ui_owns_pointer(&self.editor, &gfx.egui_ctx, self.editor.cursor);
        let pivot = self.editor.canvas_pivot;

        // Settings → Input & pen is a live reading of this stream, so every
        // event is noted here — before the match, so a branch that returns
        // early still counts, and before dispatch, so `Editor::sample` has a
        // sample of its own to write the resolved pressure onto. Observation
        // only; see `inputlog`.
        self.editor.note_input(&event);

        match event {
            // Never an immediate exit. Closing the window is the one gesture
            // that can discard *every* open document at once, so it is refused
            // until each one with unsaved work has been accounted for — and the
            // question has to be answerable with "actually, no".
            WindowEvent::CloseRequested => {
                if self.editor.unsaved_documents().is_empty() {
                    event_loop.exit();
                } else {
                    self.editor.ui.quit_prompt = true;
                    gfx.window.request_redraw();
                }
            }

            // A zero on either axis skips the whole handler: wgpu refuses a
            // zero-area configure outright, and a window with no area is one
            // there is nothing to draw on anyway, so the surface keeps the
            // configuration it had. Wayland reports one while a window is
            // being mapped.
            //
            // No surface texture can be alive here — `render` presents or
            // drops its own before returning — so this may configure at once,
            // and going through `reconfigure_surface` also drops any request a
            // suboptimal frame left behind. That request named the *old* size.
            WindowEvent::Resized(size) => {
                if size.width > 0 && size.height > 0 {
                    gfx.config.width = size.width;
                    gfx.config.height = size.height;
                    gfx.reconfigure_surface();
                    gfx.window.request_redraw();
                }
            }

            // Dropping a file on the window is the gesture people already have
            // for this, and it reaches exactly the same importer the File menu
            // does — including its refusals, so an unsupported format explains
            // itself here too rather than silently doing nothing.
            WindowEvent::DroppedFile(path) => self.open_path(&path),

            WindowEvent::ModifiersChanged(m) => {
                let was_alt = self.modifiers.alt_key();
                self.modifiers = m.state();
                let alt = self.modifiers.alt_key();
                if alt != was_alt {
                    self.set_brush_resize(alt && !ui_has_pointer);
                }
            }

            // Losing the window ends every gesture, because **no gesture may
            // be left in a state only a release could end** — and after this
            // event no release is coming. A modifier let go of while another
            // window has the keyboard never reaches us, and neither does a
            // button.
            WindowEvent::Focused(false) => {
                // Alt can otherwise be "held" for ever after an Alt-Tab, so the
                // resize would still be live — and its circle still on the
                // canvas — when the window came back.
                self.set_brush_resize(false);
                // Space is the identical failure by a route `shortcuts::direct`
                // structurally cannot see: that rule keeps a *release* always
                // landing, and here there is no release at all. Come back from
                // an Alt-Tab and the pan override is armed with no key down.
                self.editor.space_down = false;
                // A stroke is the expensive one. Alt-Tab mid-stroke and
                // `Interaction::Drawing` and `stroke.is_active()` both stay set
                // for ever: `render`'s `quiet` requires both to be clear and
                // `Autosave::next_due` answers `None` while it is not, so **the
                // document is never autosaved again for the rest of the
                // session** — silently, with its tab still showing an unsaved
                // dot. Finish rather than cancel, for the reason
                // `gesture::supersedes_stroke` gives: the artist drew a visible
                // mark.
                self.finish_stroke();
                // And the gestures that carry no stroke behind them. `quiet`
                // reads `Interaction` as well, so a pan or a zoom abandoned by
                // an Alt-Tab stalls the autosave exactly as a stroke does —
                // `finish_stroke` clears this itself, but only when there was a
                // stroke to finish.
                //
                // Deliberately *not* `cancel_selection_draft`: that takes the
                // draft away, and a polygon spans several clicks, so Alt-Tabbing
                // to look at a reference is precisely when somebody has one
                // half-drawn. Idle with a draft standing is an ordinary state
                // rather than a broken one — it is what a middle-drag to pan
                // already produces, and `pointer_moved`'s `Idle` arm keeps the
                // rubber band following the pointer.
                self.editor.interaction = Interaction::Idle;
            }

            // Switching input language changes what every key prints, and
            // therefore what every shortcut label should say. Taking the window
            // back is one of the two moments that follows such a switch; a key
            // press is the other, below. Both are on the input path
            // deliberately — see `keylayout`'s module docs for why the check
            // must not be on the drawing path instead.
            WindowEvent::Focused(true) => keylayout::forget_if_changed(),

            WindowEvent::KeyboardInput { event, .. } => {
                keylayout::forget_if_changed();
                // Punctuation dispatches on what the user's layout *prints*,
                // not on the US position winit names it by — otherwise Ctrl and
                // the key marked `+` zooms out on half the keyboards in Europe.
                // See `shortcuts::key_for_text`. A named or dead key reports no
                // character and keeps its position.
                let typed = match &event.logical_key {
                    winit::keyboard::Key::Character(text) => Some(text.as_str()),
                    _ => None,
                };
                if let PhysicalKey::Code(code) = event.physical_key
                    && self.handle_keys(shortcuts::typed_key(code, typed), event.state.is_pressed())
                    && let Some(g) = self.gfx.as_ref()
                {
                    g.window.request_redraw();
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                let pos = Vec2::new(position.x as f32, position.y as f32);
                // A mouse event, so a mouse is what is driving the pointer:
                // the pen's dot gives way to the ordinary arrow. A pen sends
                // none of these — see `Editor::pen_pointer`.
                self.editor.pen_pointer = false;
                if self.pointer_moved(pos, None)
                    && let Some(g) = self.gfx.as_ref()
                {
                    g.window.request_redraw();
                }
            }

            WindowEvent::MouseInput { state, button, .. } => {
                let pressed = state == ElementState::Pressed;
                // A button is a mouse's, whatever moved the pointer last.
                self.editor.pen_pointer = false;
                // Middle-drag and space-drag always pan, whatever tool is
                // selected — muscle memory should not depend on the rail. Space
                // is the *left* button's override; a middle press already pans.
                let pan_button = button == MouseButton::Middle;
                let pos = self.editor.cursor;

                match (pressed, button) {
                    (true, MouseButton::Left | MouseButton::Middle) => {
                        self.pointer_pressed(
                            pos,
                            // A mouse has no pressure sensor. `PressureModel`
                            // is what turns that into a full-pressure stroke.
                            None,
                            gesture::Pointer {
                                tool: self.editor.ui.tool,
                                ui_owns: ui_has_pointer,
                                alt: self.modifiers.alt_key(),
                                space: self.editor.space_down && !pan_button,
                                pan_button,
                                // A button, not a contact — which is the whole
                                // of how Alt keeps meaning the eyedropper here.
                                contact: false,
                                resizing: self.editor.brush_resize.is_some(),
                            },
                        );
                    }
                    // Any other button still ends the brush-size drag: it is
                    // Alt with *nothing* down. Without this an Alt-click would
                    // pick a colour with the resize still live, so the
                    // eyedropper would leave a circle on the canvas and the next
                    // flick of the wrist would silently rescale the brush.
                    (true, _) => self.set_brush_resize(false),
                    // The middle button pans and nothing else, so its release
                    // has nothing to finish — but it may only end the pan it
                    // began. Writing `Idle` unconditionally is the same defect
                    // `gesture::supersedes_stroke` fixes, arriving from the
                    // other direction: middle-press to pan, left-press to draw,
                    // then let the middle button go, and this cleared the
                    // `Drawing` the stroke was dispatching on, so the left
                    // button coming up never finished it. That fix is what
                    // repairs the sequence; this is what stops the arm being
                    // able to break it again.
                    (false, MouseButton::Middle) => {
                        if self.editor.interaction == Interaction::Panning {
                            self.editor.interaction = Interaction::Idle;
                        }
                    }
                    (false, MouseButton::Left) => self.pointer_released(pos, false),
                    (false, _) => {}
                }
                if let Some(g) = self.gfx.as_ref() {
                    g.window.request_redraw();
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                if ui_has_pointer {
                    return;
                }
                // Panning wants a distance and zooming wants a count of
                // notches. A wheel reports the second and a trackpad the first,
                // so both are worked out and each branch takes the one it means
                // — a trackpad's own pixels give it the fine-grained pan it is
                // for, rather than a distance rounded through a notch count.
                let scale = self.editor.pixels_per_point.max(1e-3);
                let (notches, pixels) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => {
                        (Vec2::new(x, y), Vec2::new(x, y) * WHEEL_PAN_POINTS * scale)
                    }
                    MouseScrollDelta::PixelDelta(p) => {
                        let px = Vec2::new(p.x as f32, p.y as f32);
                        (px / WHEEL_PIXELS_PER_NOTCH, px)
                    }
                };

                if self.modifiers.control_key() || self.modifiers.super_key() {
                    // Zoom keeps the wheel, one modifier along. It is where
                    // every browser and document viewer puts it, and it is the
                    // only one of the three that has somewhere to anchor: the
                    // pointer.
                    let factor = WHEEL_ZOOM_STEP.powf(notches.y);
                    self.editor
                        .camera
                        .zoom_at(self.editor.cursor, factor, pivot);
                } else {
                    // Scrolling up shows what is above, which means the picture
                    // moves *down* — `pan_by_screen` takes the movement of the
                    // canvas, so the sign is already right. Shift swaps the
                    // axis without swapping that convention, so a roll upwards
                    // goes left.
                    //
                    // A horizontal wheel is honoured whether Shift is held or
                    // not, and reversed: rolling right is asking to see what is
                    // to the right, so the canvas goes left.
                    let by = if self.modifiers.shift_key() {
                        Vec2::new(pixels.y, 0.0)
                    } else {
                        Vec2::new(-pixels.x, pixels.y)
                    };
                    self.editor.camera.pan_by_screen(by);
                }
                if let Some(g) = self.gfx.as_ref() {
                    g.window.request_redraw();
                }
            }

            WindowEvent::Touch(touch) => {
                let pos = Vec2::new(touch.location.x as f32, touch.location.y as f32);
                // winit reports Force in either normalised or calibrated form;
                // `normalized` flattens both to 0..=1.
                let reported = touch.force.map(|f| f.normalized() as f32);
                // The one place to look when a tablet does nothing:
                // `RUST_LOG=umber_app=trace` says whether the pen is reaching
                // the application at all, and with what pressure. A driver in
                // "mouse mode" sends no touches and shows nothing here.
                log::trace!(
                    "touch {:?} id={} at {pos:?} force={reported:?}",
                    touch.phase,
                    touch.id
                );
                // Whatever else this event turns out to be, it came from a pen
                // or a finger rather than a mouse — which is what the canvas
                // draws its own cursor for.
                self.editor.pen_pointer = true;

                // Which of the six things a touch event can be. In
                // `gesture::contact` rather than in a chain of `if`s here,
                // because the two rules it carries — a `Moved` for an unknown
                // id is a hover, and a second contact is a pinch — were both
                // bugs, and neither can be reproduced on a machine with no
                // tablet. The counts are taken *before* the maps are touched,
                // so `down` includes this contact for a `Started`.
                let known = self.editor.touches.contains_key(&touch.id);
                let down = self.editor.touches.len() + usize::from(!known);
                let owner = self.editor.drawing_touch == Some(touch.id);

                match gesture::contact(touch.phase, down, known, owner) {
                    gesture::Contact::Press => {
                        self.editor.touches.insert(touch.id, pos);
                        // Against the touch's *own* position. It carries one and
                        // `Editor::cursor` does not follow it, so the mouse's
                        // answer would be about somewhere else entirely — see
                        // `ui_owns_pointer`. No window means no canvas, so the
                        // interface owns it by default.
                        let ui_owns = self
                            .gfx
                            .as_ref()
                            .is_none_or(|g| ui_owns_pointer(&self.editor, &g.egui_ctx, pos));
                        // The same decision the mouse press makes, from the same
                        // function. It used to be a second `match` on the tool
                        // here, which is how the Pan tool, the Zoom tool, Alt for
                        // the eyedropper and Alt for the brush-size drag all came
                        // to be reachable only by mouse — a pen produces none of
                        // the events the arm above handles.
                        let decision = self.pointer_pressed(
                            pos,
                            reported,
                            gesture::Pointer {
                                tool: self.editor.ui.tool,
                                ui_owns,
                                // A keyboard modifier reaches a pen user exactly
                                // as it reaches a mouse user, and this arm used
                                // to consult neither.
                                alt: self.modifiers.alt_key(),
                                space: self.editor.space_down,
                                // No buttons on the glass.
                                pan_button: false,
                                contact: true,
                                resizing: self.editor.brush_resize.is_some(),
                            },
                        );
                        // A press the interface took is not this contact's to
                        // follow. Deliberately *not* treated as a pinch either:
                        // one finger on a panel is not a gesture, and cancelling
                        // the stroke there threw away a stroke the other hand
                        // was in the middle of.
                        if decision != gesture::Press::Ignored {
                            self.editor.drawing_touch = Some(touch.id);
                        }
                    }
                    gesture::Contact::Pinch => {
                        self.editor.touches.insert(touch.id, pos);
                        // A second finger means the gesture was a pinch, not a
                        // stroke. Abandon whatever the first finger was making —
                        // all of them are gestures that must not half-happen
                        // because a hand landed on the glass. The transform's
                        // *float* survives it and only the drag is dropped: the
                        // picture is still there to be moved, and throwing it
                        // away because somebody pinched to look closer at it
                        // would be the opposite of what they asked for.
                        self.cancel_stroke();
                        self.editor.cancel_selection_draft();
                        // `Editor::transform_release` rather than this type's: a
                        // second finger landing must drop the drag *and* the
                        // pending "put it down", not carry it out. Nobody
                        // pinching to look closer meant to commit.
                        self.put_down_at = None;
                        self.editor.transform_release();
                        self.editor.drawing_touch = None;
                        // A pan or a zoom the first contact had begun goes with
                        // the rest of it. `cancel_stroke` resets this only where
                        // there was a stroke to cancel, and now that the Pan and
                        // Zoom tools answer a single contact there is a live
                        // `Interaction` here that no stroke ever produced — left
                        // set, it would go on panning off the second finger's
                        // every move.
                        self.editor.interaction = Interaction::Idle;
                        self.update_pinch();
                    }
                    gesture::Contact::Hover => {
                        // A pen in range and off the glass. It is not a contact
                        // and is deliberately not recorded as one — see
                        // `gesture::contact` — but *where the pen is* is worth
                        // having, and it goes through the same body a mouse move
                        // takes, because a hover is exactly a mouse move with
                        // nothing held. The two things that live in that state —
                        // the brush-size circle following the hand, and a
                        // polygon's rubber band between clicks — used to stand
                        // still under a pen because this branch recorded the
                        // position and returned.
                        let previous = self.editor.last_cursor;
                        let wants_frame = self.pointer_moved(pos, None);
                        // `last_cursor` is the previous point of a *gesture* —
                        // what the pan and zoom drags measure against, and what a
                        // stroke's speed and therefore its simulated pressure
                        // come from — and a pen waved about in mid-air is none of
                        // those. Put back rather than never written, so that one
                        // body stays the only place a pointer move is
                        // interpreted.
                        self.editor.last_cursor = previous;
                        // A pen sends a few hundred of these a second, so the
                        // frame is asked for only where something that moved is
                        // actually drawn — over a panel or a menu this would be
                        // repainting an identical picture.
                        //
                        // This is a *narrowing* of what already happens rather
                        // than the thing that makes the hover paint: egui-winit
                        // has already asked for a repaint on the same event, so
                        // taking this out would cost frames and lose nothing.
                        // Do not go the other way and stop the pass-through as
                        // well. Under a pen the cursor is re-derived per frame
                        // and hidden by `syscursor` per frame, so a hover that
                        // stopped producing frames over a *panel* would leave
                        // whatever the last canvas frame asked for standing —
                        // which is no cursor at all, over the controls.
                        if wants_frame || self.editor.pointer_over_canvas(pos) {
                            self.request_redraw();
                        }
                        return;
                    }
                    gesture::Contact::Drag => {
                        self.editor.touches.insert(touch.id, pos);
                        // Dispatched on the `Interaction` the press set, not on
                        // the tool — which is what lets a contact drive the pan
                        // and the zoom, and what stops this arm being a second
                        // reading of what a drag means.
                        self.pointer_moved(pos, reported);
                    }
                    gesture::Contact::Pinching => {
                        self.editor.touches.insert(touch.id, pos);
                        self.update_pinch();
                    }
                    gesture::Contact::Release => {
                        self.editor.touches.remove(&touch.id);
                        // The float stays up, exactly as it does when a mouse
                        // button comes up: the box is still there to be dragged
                        // again — unless the tap was a click outside it, which
                        // puts it down.
                        self.pointer_released(pos, true);
                        self.editor.drawing_touch = None;
                        self.editor.pinch = None;
                    }
                    gesture::Contact::Lift => {
                        self.editor.touches.remove(&touch.id);
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

/// Put a patch back on the canvas and hand back the patch that undoes *that*.
///
/// The one place undo and redo differ is which stack the result goes on, so the
/// pixels are moved here rather than twice. Piece by piece, over exactly the
/// rectangles that were captured: the pixels between them were never part of
/// the edit, and reading or writing them would be work in proportion to a
/// bounding box this whole scheme exists to stop paying for.
///
/// The read blocks, once, for all the pieces together. Acceptable on an
/// explicit undo and nowhere near the drawing loop — the same rule the capture
/// at pointer-up lives by.
fn swap_patch(canvas: &mut CanvasRenderer, gpu: &Gpu, patch: &PixelPatch) -> PixelPatch {
    let rects: Vec<PixelRect> = patch.pieces().iter().map(|p| p.rect).collect();
    let current = canvas.read_layer_pieces(&gpu.device, &gpu.queue, patch.slot, &rects);
    for piece in patch.pieces() {
        canvas.write_layer_rect(&gpu.queue, patch.slot, piece.rect, &piece.bytes());
    }
    let pieces = rects
        .iter()
        .zip(current)
        .map(|(rect, bytes)| PatchPiece::new(*rect, bytes))
        .collect();
    PixelPatch::from_pieces(patch.rect, patch.slot, pieces)
}

/// The rule [`App::selection_op`] applies, as a pure function of what was held
/// and what the strip is set to.
///
/// A free function for the reason `gesture::press` is one: the interesting part
/// is a small matrix, and the matrix is only checkable if it can be stated
/// without a window and without a `winit::Modifiers`. It stays here rather than
/// in `umber_core::selection` because *which* modifier means which is the
/// interface's decision — it has to be reconciled with what Alt already does on
/// this canvas — and that is what `SelectionOp`'s own docs say.
fn combined_selection_op(add: bool, subtract: bool, setting: SelectionOp) -> SelectionOp {
    match (add, subtract) {
        (true, true) => SelectionOp::Intersect,
        (true, false) => SelectionOp::Add,
        (false, true) => SelectionOp::Subtract,
        (false, false) => setting,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Every writer of a `SaveLayer` states its effects, and this is a text
    /// guard on purpose.**
    ///
    /// There are two — Save, here, and the autosave — and both build the
    /// struct with `..SaveLayer::new(…)`, which **defaults** `effects` to
    /// empty. So deleting the field from either compiles and passes, and the
    /// failure is silent in the worst way: the document opens with its effects
    /// and is written back without them, which is precisely the
    /// open-and-save-loses-it failure `umber-version` 3 was raised to prevent,
    /// reproduced inside the build that raised it and invisible to the version
    /// gate because that gate is `version > VERSION`.
    ///
    /// The autosave's half is checked by *behaviour* —
    /// `the_autosave_writes_the_effects_the_snapshot_was_taken_with` reopens
    /// the file it wrote. Save's cannot be: it reads every layer back off the
    /// GPU first, so exercising it needs a device and a whole document. What
    /// is left is the shape, and the shape is worth pinning because the rule is
    /// about *every* such writer rather than about one line — a third one
    /// arrives already covered.
    ///
    /// **Wiring one and not the other would be worse than wiring neither**, and
    /// that is why this insists on both at once rather than on Save alone: an
    /// effect surviving or not depending on whether Save or the five-minute
    /// timer last touched the file is not a rule anybody can learn, where
    /// losing them consistently is at least a bug somebody reports.
    ///
    /// **Its reach is uneven and that is measured, not assumed.** Deleting
    /// Save's line fails this; deleting the autosave's does not, because
    /// `autosave.rs` names the field three times outside its tests — on
    /// `LayerMeta`, in `snapshot` and in `run_task` — so a lower bound of one
    /// literal is still met. That writer is covered by *behaviour* instead
    /// (`the_autosave_writes_the_effects_the_snapshot_was_taken_with` fails on
    /// the same mutation), which is the stronger guard and the reason the
    /// weaker one is only asked to cover the writer that cannot have it.
    #[test]
    fn every_writer_of_a_save_layer_states_its_effects() {
        // **Everything from `#[cfg(test)]` on is cut off first, and that is
        // not tidiness.** This test's own body names both strings it counts,
        // and its failure message named one of them — so the first draft read
        // two `SaveLayer {` in this file where there is one, and two
        // `effects:` where there is one, and passed on a coincidence. It did
        // still fail under the mutation, by 1 against 2, which is exactly how
        // a guard that passes for the wrong reason survives review. A text
        // guard has to be told not to read itself.
        for (file, whole) in [
            ("app.rs", include_str!("app.rs")),
            ("autosave.rs", include_str!("autosave.rs")),
        ] {
            let source = whole.split("#[cfg(test)]").next().unwrap_or(whole);
            let literals = source.match_indices("SaveLayer {").count();
            assert!(literals > 0, "{file} builds no SaveLayer any more");
            let stated = source.match_indices("effects:").count();
            assert!(
                stated >= literals,
                "{file} builds {literals} SaveLayer(s) outside its tests and \
                 names the field {stated} time(s); `..SaveLayer::new` defaults \
                 it to empty, so the one that does not name it drops a layer's \
                 effects in silence"
            );
        }
    }

    #[test]
    fn a_modifier_overrides_the_strips_setting_for_one_gesture_and_no_longer() {
        // Every cell, against every setting. The two things this is here to
        // pin: nothing held is the setting *whatever* it is — a modifier
        // changes what one gesture does and never what the strip says — and
        // the pair together is Intersect rather than whichever of the two won
        // an `if`/`else if`, which is what the old two-armed version would have
        // done had Intersect been bolted onto it.
        for setting in SelectionOp::ALL {
            assert_eq!(combined_selection_op(false, false, setting), setting);
            assert_eq!(
                combined_selection_op(true, false, setting),
                SelectionOp::Add
            );
            assert_eq!(
                combined_selection_op(false, true, setting),
                SelectionOp::Subtract
            );
            assert_eq!(
                combined_selection_op(true, true, setting),
                SelectionOp::Intersect
            );
        }
    }

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
