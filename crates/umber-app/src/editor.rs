//! Editor state — everything that is not a GPU resource or a window.

use crate::colorpicker::{PickerMode, WheelAngles, WheelShape};
use crate::dock::Layout;
use crate::session::{DocId, DocumentState, Session};
use crate::settings::SettingsTab;
use crate::tabs::Notice;
use crate::theme::{Accent, ThemeKind};
use glam::Vec2;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use umber_core::{
    Brush, BrushMode, BrushPreset, Camera, Clip, Color, Document, Handle, History, Hsv, InputPoint,
    LayerStack, Selection, SelectionDraft, SelectionMode, SelectionOp, StrokeBuilder, TipMask,
    Transform,
    input::{PressureModel, PressureSource},
};
use umber_render::{LayerDraw, StrokeStyle};

/// How near the first vertex a click has to land to close a polygon, in
/// *screen* pixels. Divided by the zoom at the point of use.
const SELECT_CLOSE_PIXELS: f32 = 10.0;

/// What the pointer is currently doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interaction {
    Idle,
    Drawing,
    /// A selection outline is being drawn. Distinct from `Drawing` because
    /// nothing about it touches the stroke builder or the scratch surface —
    /// and because the autosave's "is it quiet?" test must count it as busy.
    Selecting,
    Panning,
    Zooming,
}

/// A brush-size drag in progress: Alt held down with no button pressed.
///
/// Not an [`Interaction`], because it is not what the *pointer* is doing —
/// nothing is held, the canvas is not being drawn on or moved, and letting go
/// of Alt over a panel has to end it wherever the pointer happens to be. It is
/// a modifier's state, and it lives and dies with `ModifiersChanged`.
#[derive(Clone, Copy, Debug)]
pub struct BrushResize {
    /// Where the pointer was when Alt went down, in physical window pixels.
    /// The drag is measured from here, and the preview circle is centred here.
    pub origin: Vec2,
    /// The size the brush had at that moment. The drag is absolute against
    /// this, so coming back to the origin comes back to exactly this size.
    pub from: f32,
}

/// The selected tool. Brush and eraser paint, select marks out where they may,
/// transform moves what they marked, and pan and zoom navigate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Brush,
    Eraser,
    Select,
    Transform,
    Pan,
    Zoom,
}

impl Tool {
    pub fn paints(self) -> bool {
        matches!(self, Self::Brush | Self::Eraser)
    }
}

/// Pixels picked up off a layer — or pasted onto one — being moved about.
///
/// Transient like [`Editor::stroke`], and for the same reason it sits above the
/// `--- documents ---` line: every path that would leave the document behind
/// puts it down first, so it never has to travel. Its pixels live in the
/// renderer; what is here is only where they have been dragged to.
#[derive(Clone, Copy, Debug)]
pub struct Floating {
    pub xf: Transform,
    /// The layer slot the pixels belong to, snapshotted for the same reason
    /// [`Editor::stroke_slot`] is: selecting another layer mid-gesture must not
    /// land the commit somewhere else.
    pub slot: u32,
    /// True when the pixels were taken *out* of the layer, so the commit has to
    /// restore the hole as well as the destination. A paste is the other case.
    pub lifted: bool,
    /// What the pointer has hold of, and the document point it grabbed at.
    /// Absolute against that point rather than accumulated per event — see
    /// [`Transform::drag`].
    pub drag: Option<(Handle, Vec2)>,
}

/// How near a handle a press has to land, in *screen* pixels. Divided by the
/// zoom at the point of use, exactly as the polygon lasso's close distance is:
/// a fixed document distance would be impossible to hit at 10% and impossible
/// to avoid at 800%.
pub const HANDLE_GRAB_PIXELS: f32 = 8.0;

/// Presentation state — what the interface looks like, not what the document
/// contains. Kept apart from the document so it can be persisted separately
/// later without dragging artwork into a preferences file.
#[derive(Clone, Copy, Debug)]
pub struct UiState {
    pub theme: ThemeKind,
    /// Which of the design's four accents re-hues the palette. Separate from
    /// `theme` because it is orthogonal to it — either accent works on either
    /// surface, so folding them together would mean four more themes.
    pub accent: Accent,
    pub pressure_open: bool,
    pub tool: Tool,
    /// Which outline the selection tool draws. One tool with a mode rather
    /// than three tools: see `umber_core::selection`.
    pub selection_mode: SelectionMode,
    pub picker: PickerMode,
    pub wheel_shape: WheelShape,
    /// Whether the wheel's triangle turns to follow the hue. Meaningless for the
    /// square, which has no corner that is the hue to keep beside the marker.
    pub wheel_rotates: bool,
    /// How far each wheel centre is turned from its neutral pose, when the hue
    /// is not deciding it. One angle per shape — see [`WheelAngles`].
    pub wheel_angles: WheelAngles,
    pub brush_editor_open: bool,
    pub brush_tab: BrushTab,
    pub settings_open: bool,
    pub settings_tab: SettingsTab,
    /// The module library — every dockable module, and the way to put one back
    /// after it has been removed from the layout.
    pub module_library_open: bool,
    /// Help, About. The update prompts raise themselves and are not here.
    pub about_open: bool,
    /// Tab whose close is waiting on confirmation, if any.
    pub close_prompt: Option<usize>,
    /// The window has been asked to close and something would be lost.
    ///
    /// A flag rather than a list of tabs because the list is recomputed from
    /// the session every frame — a tab could be saved or closed while the
    /// prompt is up, and a snapshot taken when it opened would go on naming a
    /// document that is no longer at risk.
    pub quit_prompt: bool,
    /// Which row of the brush editor's Inputs list is open for editing.
    ///
    /// An index rather than a copy of the entry, because the list is short and
    /// the entry is the brush's — a copy would need writing back and would go
    /// stale the moment a row above it was deleted.
    pub modulation: usize,
    /// Whether a save carries the undo history into the document.
    ///
    /// A preference rather than a fixed policy because it is the one setting
    /// here that trades file size for a feature: a bounded slice of the history
    /// goes into the archive, which on a heavy painting session is tens of
    /// megabytes beside a document that might otherwise be a few. On by
    /// default, because a history nobody knows to switch on is one nobody gets,
    /// and because the cost is bounded at both ends — see
    /// `umber_core::docformat::history`.
    pub save_history: bool,
}

/// Tabs of the brush editor dialog.
///
/// The design lists six sections — Tip, Dynamics, Texture, Scatter, Wet edges,
/// Stabiliser — and these are the five Umber can fill. Wet edges has no engine
/// behind it, so it is not drawn at all rather than drawn empty; Stabilisation
/// is one slider and rides on Tip rather than getting a section to itself.
///
/// `Blending` and `Inputs` are not among the design's names. Colour pickup
/// needs a home and none of the six is one: filing it under "Wet edges" would
/// be borrowing a term that means something else in every application that has
/// it. `Inputs` is the modulation table — everything that drives the brush and
/// is not pressure — and `Dynamics` is already taken by the pressure curves,
/// which is exactly the distinction the two names have to draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrushTab {
    Tip,
    Dynamics,
    Inputs,
    Scatter,
    Texture,
    Blending,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            theme: ThemeKind::Graphite,
            accent: Accent::Umber,
            pressure_open: true,
            tool: Tool::Brush,
            selection_mode: SelectionMode::Rectangle,
            picker: PickerMode::Wheel,
            wheel_shape: WheelShape::Triangle,
            // What the picker has always done, and what the design draws.
            wheel_rotates: true,
            // Zero is the pose every build before the angle existed drew.
            wheel_angles: WheelAngles::default(),
            brush_editor_open: false,
            brush_tab: BrushTab::Tip,
            settings_open: false,
            settings_tab: SettingsTab::Themes,
            module_library_open: false,
            about_open: false,
            close_prompt: None,
            quit_prompt: false,
            modulation: 0,
            save_history: true,
        }
    }
}

pub struct Editor {
    /// The live document. Every other open document is parked in its tab —
    /// see [`crate::session`] for why the active one stays out here.
    pub doc: Document,
    pub camera: Camera,
    pub brush: Brush,
    pub color: Color,
    /// Background colour, swapped with `color` by X.
    pub secondary: Color,
    /// The picker's own state. Deriving hue from `color` each frame would lose
    /// it whenever saturation or value reaches zero.
    pub hsv: Hsv,
    pub presets: Vec<BrushPreset>,
    pub active_preset: Option<usize>,
    /// The bitmap tip the brush in hand stamps, or `None` for the procedural
    /// round dab.
    ///
    /// Resolved once, when a preset is selected, rather than looked up per
    /// stroke: `BrushPreset::tip` is a *name*, and the masks live in the user's
    /// library, which the drawing path has no business reaching into. The `Arc`
    /// is what `CanvasRenderer::set_tip` compares to decide whether the tip
    /// already on the GPU is the one wanted.
    pub tip: Option<Arc<TipMask>>,
    /// Every mask the user's library holds, by name — what [`Editor::tip`] is
    /// resolved against. Filled in by `brushlib::resync`, which is also what
    /// keeps `presets` in step.
    pub tips: BTreeMap<String, Arc<TipMask>>,
    pub layers: LayerStack,
    /// Where the live document lets an edit land, or `None` for all of it.
    ///
    /// An `Arc` because the renderer compares it by identity to decide whether
    /// the mask on the GPU is still the right one — the same check
    /// `Editor::tip` gets, and for the same reason: a selection is up to a
    /// megabyte of coverage, and comparing it every stroke would cost more
    /// than the upload it saves.
    ///
    /// Per-document, so it lives in [`DocumentState`] as well as here.
    pub selection: Option<Arc<Selection>>,
    /// Every open document, and which of them the fields above belong to.
    pub session: Session,
    /// A message that has to reach the user rather than the log — an import
    /// that could not be represented in full, or one that failed outright.
    pub notice: Option<Notice>,
    pub ui: UiState,
    /// State of the New document and Canvas settings dialogs. Kept out of
    /// [`UiState`] so that stays `Copy` — it holds a colour picker's HSV, which
    /// has the same reason to be its own source of truth here as in the Colour
    /// panel. Seeded from the live document when a dialog opens.
    pub canvas_form: crate::canvasdlg::CanvasForm,
    /// The update check: whether it runs, what it last said, and how this copy
    /// was installed. Kept out of [`UiState`] because it holds a channel and a
    /// downloaded release, neither of which is `Copy`.
    pub updates: crate::update::Updates,
    /// The autosave: its schedule, the capture in flight and the thread that
    /// writes. Out of [`UiState`] for the same reason `updates` is — it holds
    /// channels and a map of every open document.
    pub autosave: crate::autosave::Autosave,
    /// True once the application has been asked to close and every document
    /// with unsaved work has been accounted for.
    ///
    /// A flag rather than a call to `event_loop.exit()` because the quit prompt
    /// is drawn from `ui::draw`, which has no `ActiveEventLoop` — the same
    /// arrangement `Updates::take_quit_request` uses for the Windows installer.
    pub quit_requested: bool,
    /// Where the dockable modules are. Kept out of [`UiState`] so that stays
    /// `Copy`; it also has its own lifetime, being loaded from and saved to a
    /// config file rather than living only for the session.
    pub layout: Layout,
    /// Centre of the region the document is drawn in, in physical pixels.
    ///
    /// *Docked* panels take a bite out of the window, so this is not the window
    /// centre. Floating panels deliberately do not: they hover over the canvas,
    /// so moving one must not shift where a dab lands.
    pub canvas_pivot: Vec2,
    /// Size of that region, for fit-to-view.
    pub canvas_size: Vec2,
    /// The canvas scrollbars as they were last drawn, in points: horizontal
    /// then vertical, `None` where the document does not run off that edge.
    ///
    /// Recorded because a press on a bar must not also start a stroke, and the
    /// usual test cannot answer it: these sit *inside* the canvas region and are
    /// drawn in egui's background layer, so neither `pointer_over_canvas` nor
    /// the `layer_id_at` check in `app.rs` sees them. Set every frame by
    /// `ui::draw`, and an array rather than a `Vec` because it is written on the
    /// drawing path.
    pub scroll_bars: [Option<egui::Rect>; 2],
    /// The floating transform's two flip buttons as they were last drawn, in
    /// points: horizontal then vertical, `None` when no transform is up.
    ///
    /// Recorded for exactly the reason [`Editor::scroll_bars`] is, and it is
    /// the same problem: these are real buttons painted *inside* the canvas
    /// region in egui's background layer, so without this a press on one would
    /// also be a press on the canvas — which with the transform tool in hand
    /// means putting the picture down, immediately, before the flip could take
    /// effect.
    pub transform_buttons: [Option<egui::Rect>; 2],
    /// egui points per physical pixel, from the last frame. Window events
    /// arrive in physical pixels and the layout works in points, so hit-testing
    /// a cursor position against a floating panel needs the conversion.
    pub pixels_per_point: f32,

    pub stroke: StrokeBuilder,
    /// The floating transform in progress, if there is one.
    ///
    /// Transient, like [`Editor::stroke`]: everything that would leave the
    /// document behind commits it first, so it never crosses a tab switch.
    pub float: Option<Floating>,
    /// What was last copied, ready to be pasted.
    ///
    /// Genuinely session-wide rather than per-document — copying out of one tab
    /// and into another is most of what a clipboard is for — so it belongs
    /// above the `--- documents ---` line and stays there across a switch.
    pub clipboard: Option<Clip>,
    /// The selection outline being drawn, if one is. Transient like
    /// [`Editor::stroke`], and abandoned rather than carried across a tab
    /// switch — half a lasso belongs to the gesture, not to the document.
    pub selection_draft: Option<SelectionDraft>,
    /// Scratch for the outline being painted this frame.
    ///
    /// Held rather than built per frame for the reason
    /// `SelectionDraft::outline_into` takes a buffer at all: drawing the
    /// outline is the one part of the selection path that runs every frame.
    pub selection_outline: Vec<glam::Vec2>,
    /// The same ring in screen points, and the dashes cut from it.
    ///
    /// Two more buffers for the same reason, and a stronger one since the ants
    /// march: a document with a selection asks for a frame several times a
    /// second for as long as it is open, so anything this path allocates it
    /// allocates for ever. `Shape::dashed_line` returns a fresh `Vec` per ring
    /// per frame; `dashed_line_many_with_offset` fills these instead.
    pub selection_screen: Vec<egui::Pos2>,
    pub selection_dashes: Vec<egui::Shape>,
    pub history: History,
    pub pressure: PressureModel,
    /// What the pointer stream has been doing lately, for Settings → Input &
    /// pen. Transient telemetry of the *window*, not of a document — it
    /// describes the tablet plugged into this machine, and a tab switch has
    /// nothing to do with it — so it belongs above the `--- documents ---`
    /// line and is deliberately not part of [`DocumentState`].
    ///
    /// Written by [`Editor::note_input`] and [`Editor::sample`]; read only by
    /// the settings pane. Nothing on the stroke path may start reading it, or
    /// the diagnostic becomes part of what it is meant to be observing.
    pub input: crate::inputlog::InputLog,

    pub interaction: Interaction,
    /// Cursor in physical window pixels.
    pub cursor: Vec2,
    pub last_cursor: Vec2,
    /// True when a pen, rather than a mouse, is driving the pointer.
    ///
    /// The signal is which *kind* of event last moved it. A pen reaches winit
    /// as `WindowEvent::Touch` — Windows delivers it through `WM_POINTER`, and
    /// winit consumes those messages rather than letting the system promote
    /// them to legacy mouse ones, so a pen produces no `CursorMoved` at all.
    /// See the pressure notes in CLAUDE.md, which is the same fact read the
    /// other way round.
    ///
    /// Latched, because the canvas is painted between events and has to know
    /// what it is drawing a cursor for; and cleared by any real mouse event, so
    /// that putting the pen down and taking hold of the mouse hands the arrow
    /// straight back.
    pub pen_pointer: bool,
    /// Space held — temporary pan modifier.
    pub space_down: bool,
    /// Where a zoom-tool drag started; zooming keeps this point pinned.
    pub zoom_anchor: Vec2,
    /// The brush-size drag, while Alt is held with no button down.
    ///
    /// `Some` is also what draws the preview circle, so there is one thing to
    /// look at for "is this gesture live" rather than a flag and a state that
    /// could disagree.
    pub brush_resize: Option<BrushResize>,

    /// Brush settings captured at stroke start. The user can change the colour
    /// mid-stroke via the UI; the stroke must still commit with what it began
    /// with, or the preview and the committed result disagree.
    pub stroke_style: StrokeStyle,
    /// Layer slot the stroke started on. Captured because the user can select a
    /// different layer mid-stroke, and the stroke must land where it began.
    pub stroke_slot: u32,

    /// Touch points currently down, for pinch handling.
    pub touches: HashMap<u64, Vec2>,
    /// The touch that owns the current stroke.
    pub drawing_touch: Option<u64>,
    /// Pinch state: distance and midpoint at the previous sample.
    pub pinch: Option<(f32, Vec2)>,

    start: Instant,
    last_sample_time: f64,
    pub frame_times: [f32; 60],
    pub frame_cursor: usize,
}

impl Default for Editor {
    fn default() -> Self {
        let doc = Document::default();
        Self {
            doc,
            camera: Camera {
                center: doc.size_vec2() * 0.5,
                zoom: 1.0,
            },
            brush: Brush::default(),
            color: Color::from_srgb_u8(20, 20, 24, 255),
            secondary: Color::WHITE,
            hsv: Color::from_srgb_u8(20, 20, 24, 255).to_hsv(),
            presets: umber_core::preset::builtin().to_vec(),
            active_preset: None,
            tip: None,
            tips: BTreeMap::new(),
            layers: LayerStack::new(),
            session: Session::default(),
            notice: None,
            ui: UiState::default(),
            canvas_form: crate::canvasdlg::CanvasForm::default(),
            updates: crate::update::Updates::default(),
            autosave: crate::autosave::Autosave::default(),
            quit_requested: false,
            // Read here rather than in `app.rs` so the window-creation path,
            // which several things already contend over, stays untouched.
            layout: Layout::load_or_default(),
            canvas_pivot: Vec2::ZERO,
            canvas_size: Vec2::ONE,
            scroll_bars: [None, None],
            transform_buttons: [None, None],
            pixels_per_point: 1.0,
            selection: None,
            stroke: StrokeBuilder::new(),
            float: None,
            clipboard: None,
            selection_draft: None,
            selection_outline: Vec::new(),
            selection_screen: Vec::new(),
            selection_dashes: Vec::new(),
            history: History::default(),
            pressure: PressureModel::default(),
            input: crate::inputlog::InputLog::default(),
            interaction: Interaction::Idle,
            cursor: Vec2::ZERO,
            last_cursor: Vec2::ZERO,
            pen_pointer: false,
            space_down: false,
            zoom_anchor: Vec2::ZERO,
            brush_resize: None,
            stroke_style: StrokeStyle::default(),
            stroke_slot: 0,
            touches: HashMap::new(),
            drawing_touch: None,
            pinch: None,
            start: Instant::now(),
            last_sample_time: 0.0,
            frame_times: [0.0; 60],
            frame_cursor: 0,
        }
    }
}

impl Editor {
    pub fn now(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    pub fn screen_to_doc(&self, screen: Vec2) -> Vec2 {
        self.camera.screen_to_doc(screen, self.canvas_pivot)
    }

    /// Build an input sample, resolving pressure through the current model.
    pub fn sample(&mut self, screen: Vec2, reported: Option<f32>) -> InputPoint {
        let now = self.now();
        let dt = (now - self.last_sample_time).max(0.0);
        self.last_sample_time = now;

        let doc = self.screen_to_doc(screen);
        // Speed is measured in document pixels so simulated pressure behaves
        // the same at every zoom level.
        let distance = (doc - self.screen_to_doc(self.last_cursor)).length();
        let pressure = self.pressure.resolve(reported, distance, dt);
        // Record what that one call answered. `resolve` mutates the model — it
        // carries the simulated value forward and latches whether the device
        // has been heard from this stroke — so the diagnostic must take the
        // real answer rather than ask again for a number to draw.
        self.input.note_resolved(pressure);

        InputPoint::new(doc, pressure, now)
    }

    /// Note a window event for the input diagnostic.
    ///
    /// Observation only: nothing downstream reads what this records, and the
    /// stroke path behaves identically whether Settings → Input & pen is open
    /// or not. See [`crate::inputlog`].
    pub fn note_input(&mut self, event: &winit::event::WindowEvent) {
        let now = self.now();
        self.input.note(event, now);
    }

    /// True when the layout, rather than the canvas, owns the pointer.
    ///
    /// Docked panels are egui panels, so egui's own "is the pointer over me"
    /// answers for them. Floating panels and a panel being dragged are not
    /// covered reliably enough: an [`egui::Area`] is only known to egui's hit
    /// testing at the position it had *last* frame, and a drag over open canvas
    /// is over no widget at all. Getting this wrong means a panel dragged
    /// across the canvas paints a stroke underneath itself.
    ///
    /// `screen` is in physical pixels; the layout works in egui points.
    pub fn layout_owns_pointer(&self, screen: Vec2) -> bool {
        self.layout.blocks_canvas(self.to_points(screen))
    }

    /// True when `screen` (physical pixels) is over one of the controls that
    /// sit *inside* the canvas region — the scrollbars and the floating
    /// transform's flip buttons — and are therefore not covered by
    /// [`Editor::layout_owns_pointer`].
    pub fn canvas_overlay_owns_pointer(&self, screen: Vec2) -> bool {
        let at = self.to_points(screen);
        self.scroll_bars
            .iter()
            .chain(self.transform_buttons.iter())
            .flatten()
            .any(|bar| bar.contains(at))
    }

    /// Physical window pixels to egui points.
    pub fn to_points(&self, screen: Vec2) -> egui::Pos2 {
        let scale = self.pixels_per_point.max(1e-3);
        egui::pos2(screen.x / scale, screen.y / scale)
    }

    /// True when a press at `screen` (physical pixels) belongs to the document.
    ///
    /// Derived from the canvas region itself rather than asked of egui.
    /// `Context::is_pointer_over_egui` cannot answer it: since egui 0.35's
    /// `CentralPanel` consumes the root `Ui`'s cursor, the "unused" rect it
    /// tests against is empty by the end of the pass, so it reports the pointer
    /// as over egui *everywhere* — including the middle of the canvas. That in
    /// turn makes `egui_wants_pointer_input()` true on every fresh press, which
    /// swallowed the press that starts a stroke.
    ///
    /// `canvas_pivot` and `canvas_size` are the same numbers the composite pass
    /// is given, so this test and where the dab lands cannot drift apart.
    pub fn pointer_over_canvas(&self, screen: Vec2) -> bool {
        let half = self.canvas_size * 0.5;
        let min = self.canvas_pivot - half;
        let max = self.canvas_pivot + half;
        let inside =
            screen.x >= min.x && screen.x <= max.x && screen.y >= min.y && screen.y <= max.y;
        inside && !self.layout_owns_pointer(screen) && !self.canvas_overlay_owns_pointer(screen)
    }

    /// Select a tool, keeping the brush's paint/erase mode in step.
    pub fn set_tool(&mut self, tool: Tool) {
        self.ui.tool = tool;
        // A half-drawn outline belongs to the tool that was drawing it. Through
        // `cancel_selection_draft` rather than by clearing the field, because
        // the interaction has to come back to `Idle` with it: a shortcut can
        // change tool with the button still down, and an interaction left in
        // `Selecting` with no draft to answer for it is one that nothing ever
        // ends — no autosave, and a redraw requested on every mouse move for
        // the rest of the session.
        if tool != Tool::Select {
            self.cancel_selection_draft();
        }
        match tool {
            Tool::Brush => self.brush.mode = BrushMode::Paint,
            Tool::Eraser => self.brush.mode = BrushMode::Erase,
            Tool::Select | Tool::Transform | Tool::Pan | Tool::Zoom => {}
        }
    }

    /// Adopt the picker's HSV as the painting colour.
    pub fn commit_picker(&mut self) {
        self.color = self.hsv.to_color(1.0);
    }

    /// Point the picker at a colour chosen elsewhere, preserving hue for greys.
    pub fn set_color(&mut self, color: Color) {
        let next = color.to_hsv();
        self.color = color;
        self.hsv.s = next.s;
        self.hsv.v = next.v;
        if next.s > 1e-4 {
            self.hsv.h = next.h;
        }
    }

    /// Swap foreground and background colours.
    pub fn swap_colors(&mut self) {
        std::mem::swap(&mut self.color, &mut self.secondary);
        let color = self.color;
        self.set_color(color);
    }

    /// Load a brush preset, keeping the current paint/erase mode — switching
    /// brush should not silently turn the eraser back into a brush.
    pub fn apply_preset(&mut self, index: usize) {
        let Some(preset) = self.presets.get(index) else {
            return;
        };
        let mode = self.brush.mode;
        self.brush = preset.brush;
        self.brush.mode = mode;
        // A name pointing at a mask that is not here paints round rather than
        // refusing — see `BrushPreset::tip`.
        //
        // The user's library first, then the masks Umber ships. Both hand back
        // an `Arc<TipMask>` that is stable for as long as it is reachable,
        // which is what `CanvasRenderer::set_tip`'s identity check needs; the
        // order only decides a name collision, and the user's own file winning
        // is the answer that cannot surprise anybody.
        self.tip = preset.tip.as_ref().and_then(|name| {
            self.tips
                .get(name)
                .cloned()
                .or_else(|| umber_core::tip::builtin(name).cloned())
        });
        self.active_preset = Some(index);
    }

    /// Take the bitmap tip off the brush in hand, without touching the preset
    /// it came from. Saving is what makes that stick.
    pub fn clear_tip(&mut self) {
        self.tip = None;
    }

    pub fn fit_view(&mut self) {
        self.camera = Camera::fit(self.doc.size_vec2(), self.canvas_size);
    }

    /// Zoom about the centre of the canvas region.
    ///
    /// The wheel and the zoom tool anchor on the pointer, but a keyboard zoom
    /// has no pointer to anchor on — anchoring on the cursor anyway would move
    /// the canvas under a hand that is nowhere near it, and off the edge of the
    /// window if the cursor happens to be over a panel. Anchoring on the pivot
    /// leaves `camera.center` exactly where it was, which is what
    /// "zoom in on what I am looking at" means.
    pub fn zoom_by(&mut self, factor: f32) {
        let pivot = self.canvas_pivot;
        self.camera.zoom_at(pivot, factor, pivot);
    }

    // --- documents -------------------------------------------------------
    //
    // A tab switch moves state between here and [`Session`]; nothing above
    // this line is per-document, which is the whole design. See the module
    // docs in `session.rs`.

    /// Move the live document's state out, leaving a blank stand-in behind.
    ///
    /// The stand-in never reaches the screen: every caller installs another
    /// document in the same breath. It exists because these fields are read by
    /// name all over the interface, so they cannot be an `Option`.
    fn take_document(&mut self) -> DocumentState {
        DocumentState {
            doc: std::mem::take(&mut self.doc),
            layers: std::mem::replace(&mut self.layers, LayerStack::new()),
            history: std::mem::take(&mut self.history),
            camera: self.camera,
            selection: self.selection.take(),
        }
    }

    fn install_document(&mut self, state: DocumentState) {
        self.doc = state.doc;
        self.layers = state.layers;
        self.history = state.history;
        self.camera = state.camera;
        self.selection = state.selection;
        // The gesture, unlike the selection, does not travel: it belonged to
        // the pointer, and the pointer is now over a different document.
        self.selection_draft = None;
        // Belt and braces. Every caller owes this a document with nothing
        // floating — the pixels live in the *outgoing* document's renderer, so
        // carrying the record across would leave a preview standing in front of
        // a layer in a tab nobody is looking at. `app.rs` commits it before
        // every one of these; clearing it here means a path that forgot leaves
        // an abandoned transform rather than a corrupted one.
        self.float = None;
        // The stroke that was in flight, if any, was finished by the caller
        // before the swap; this only stops a stale slot from the *previous*
        // document being carried into the next commit.
        self.stroke_slot = self.layers.active_slot();
        self.interaction = Interaction::Idle;
    }

    /// Open a document that has already been built — an import, or a blank one.
    ///
    /// Returns its id so the caller can give it GPU storage.
    pub fn open_document(
        &mut self,
        state: DocumentState,
        title: String,
        path: Option<PathBuf>,
        notes: Vec<String>,
    ) -> DocId {
        let outgoing = self.take_document();
        let id = self.session.open(title, path, outgoing);
        self.session.active_tab_mut().notes = notes;
        self.install_document(state);
        self.fit_view();
        id
    }

    /// Open a new blank document described by `doc`.
    pub fn create_document(&mut self, doc: Document) -> DocId {
        let title = self.session.next_untitled_title();
        self.open_document(DocumentState::blank(doc), title, None, Vec::new())
    }

    /// Open a new blank document like the current one.
    ///
    /// Inheriting the whole document rather than using the default is what
    /// makes the tab strip's `+` useful next to an imported one: the common
    /// reason to open a second tab is to try something at the same scale, on
    /// the same paper. File → New… asks instead.
    pub fn new_document(&mut self) -> DocId {
        self.create_document(self.doc)
    }

    /// Apply new canvas settings to the live document.
    ///
    /// Returns true when the *geometry* changed, which is the caller's cue to
    /// resize the document's textures and throw the undo history away — every
    /// patch in it is a rectangle of the old canvas, and the same reasoning
    /// applies as when a layer is deleted.
    ///
    /// The history is cleared here rather than left to the caller so it cannot
    /// be forgotten by one of two call sites.
    pub fn apply_canvas(&mut self, doc: Document) -> bool {
        let resized = doc.size != self.doc.size;
        // Only a real change is a change: pressing Apply on a dialog nobody
        // touched must not put a dot on the tab and start asking about
        // unsaved work.
        if doc != self.doc {
            self.mark_modified();
        }
        self.doc = doc;
        if resized {
            self.history.clear();
            // Its bounds are a rectangle of the old canvas and can now name
            // pixels that do not exist. Dropped rather than rescaled, for the
            // reason the history is dropped rather than remapped: a selection
            // is a statement about where the artist is working, and a
            // resampled one is a guess. `CanvasRenderer::resize` drops the
            // mask on the GPU to match.
            self.selection = None;
            self.selection_draft = None;
            // Keep the zoom, but not the ability to be looking at a part of the
            // canvas that no longer exists.
            self.camera.center = self.camera.center.clamp(Vec2::ZERO, doc.size_vec2());
        }
        resized
    }

    /// Mirror the live document, in everything but its pixels.
    ///
    /// The pixels are the renderer's — see `CanvasRenderer::flip_layers` — and
    /// the history entry is `app.rs`'s, because it has to be recorded only if
    /// the GPU work actually happened. What is here is the selection, which is
    /// geometry and belongs to the document.
    ///
    /// **Called for the flip and again for its undo**, and it is its own
    /// inverse on both halves, which is the whole reason the history can record
    /// a flip without storing a pixel. The canvas size does not change, so
    /// unlike a resize nothing recorded against this canvas stops being valid —
    /// the history and the camera are deliberately untouched.
    ///
    /// The one thing that is not exactly reversible is a selection that has
    /// already been through a boolean: its rings were traced back out of the
    /// mask and are pixel-quantised, so mirroring them re-rasterises a
    /// staircase. That is the loss `selection`'s module docs already own, and
    /// it is a one-pixel one; a mirrored *mask* would be a second rasteriser
    /// that had to agree with the first about every antialiased edge.
    pub fn flip_canvas(&mut self, axis: umber_core::FlipAxis) {
        let doc = self.doc.size;
        // A selection that mirrors to nothing cannot arise — a mirror preserves
        // area — but `flipped` is an `Option` because `from_rings` is, and
        // dropping it is the right answer if it ever does: an outline covering
        // nothing is no selection.
        self.selection = self
            .selection
            .as_deref()
            .and_then(|sel| sel.flipped(axis, doc))
            .map(Arc::new);
        // The gesture belongs to the pointer and was drawn on the picture as it
        // was. Abandoned rather than mirrored, exactly as a tab switch does —
        // and through `cancel_selection_draft` rather than by clearing the
        // field, for the reason `set_tool` gives: a shortcut can fire with the
        // button still down, and an interaction left in `Selecting` with no
        // draft to answer for it is one that nothing ever ends.
        self.cancel_selection_draft();
    }

    /// Make tab `index` the live document.
    ///
    /// Returns false when there is nothing to do, so the caller can skip the
    /// GPU work that follows a real switch.
    pub fn switch_tab(&mut self, index: usize) -> bool {
        if index >= self.session.len() || index == self.session.active_index() {
            return false;
        }
        // Taken before the live state is disturbed: if the parked state were
        // missing, the editor would otherwise be left holding the stand-in.
        let Some(incoming) = self.session.take_parked(index) else {
            log::error!("tab {index} has no parked document");
            return false;
        };
        let outgoing = self.take_document();
        self.session.park_active(outgoing);
        self.session.set_active(index);
        self.install_document(incoming);
        true
    }

    /// Close tab `index`, returning the id whose GPU storage can now be freed.
    ///
    /// The last document cannot be closed — Umber has nowhere to go with no
    /// document open, and the tab strip draws no close mark on it.
    pub fn close_tab(&mut self, index: usize) -> Option<DocId> {
        let successor = self.session.successor_of(index)?;
        if index == self.session.active_index() {
            let incoming = self.session.take_parked(successor)?;
            let closed = self.session.remove(index)?;
            // The live state belonged to the document being closed, so it is
            // dropped rather than parked.
            self.install_document(incoming);
            Some(closed.id)
        } else {
            self.session.remove(index).map(|tab| tab.id)
        }
    }

    /// Note that the live document has changed, so closing it would lose work.
    pub fn mark_modified(&mut self) {
        self.session.mark_modified();
    }

    /// Note that the live document has been written to `path`.
    pub fn mark_saved(&mut self, path: PathBuf) {
        self.session.mark_saved(path);
    }

    /// Every open document that would lose something if it went now, as tab
    /// positions.
    ///
    /// **Every** document, not only the one in front: closing the window
    /// discards all of them at once, and a prompt that named one while quietly
    /// dropping the other two would be worse than none. Recomputed on demand
    /// rather than snapshotted, so the prompt cannot go on naming a document
    /// that has since been saved.
    pub fn unsaved_documents(&self) -> Vec<usize> {
        self.session
            .tabs()
            .iter()
            .enumerate()
            .filter(|(_, tab)| tab.modified)
            .map(|(i, _)| i)
            .collect()
    }

    /// Every open document and how many texture-array slices its layers
    /// occupy. The live document is included.
    ///
    /// Used to rebuild GPU storage after the surface has been destroyed and
    /// recreated, which on Android happens whenever the app is backgrounded.
    /// The slot count has to travel with the document: a renderer is built with
    /// room for a few slices, and a document with more layers than that would
    /// come back to a texture array too shallow to commit its strokes into.
    pub fn open_documents(&self) -> Vec<(DocId, Document, u32)> {
        let live = (self.doc, self.layers.slot_capacity_needed());
        self.session
            .tabs()
            .iter()
            .map(|tab| {
                let (doc, slots) = tab.parked_storage().unwrap_or(live);
                (tab.id, doc, slots)
            })
            .collect()
    }

    pub fn begin_stroke(&mut self, point: InputPoint) {
        // Snapshot the brush: the user can change colour, opacity or layer via
        // the panel mid-stroke, but the stroke must finish as it started.
        self.stroke_style = StrokeStyle {
            color: self.color,
            opacity: self.brush.opacity,
            mode: self.brush.mode,
            // Decided once, here, from the brush this stroke started with. It
            // must not change mid-stroke: dabs already stamped without a colour
            // recorded would commit as the flat palette colour while the rest
            // smudged.
            //
            // Colour pickup is no longer the only thing that colours a dab: a
            // hue, saturation or brightness modulation does too. This must
            // agree with `StrokeBuilder::is_coloured`, which is what decides
            // which dab pipeline the frame uses.
            per_dab_color: self.brush.colours_dabs(),
        };
        self.stroke_slot = self.layers.active_slot();
        self.pressure.reset();
        // `Color` is already linear — the engine works in linear throughout —
        // so this is the same value the composite would have used.
        let paint = [self.color.r, self.color.g, self.color.b];
        self.stroke.begin(self.brush, paint, point);
        self.interaction = Interaction::Drawing;
    }

    // --- selections -------------------------------------------------------

    /// A press on the canvas with the selection tool in hand.
    ///
    /// Only the polygon can see a second press: the other two modes are one
    /// press, a drag and a release, and their draft is gone by the time
    /// another arrives.
    ///
    /// `op` is therefore read from the press that *starts* the gesture and
    /// ignored on every one after it — a polygon spans several clicks and must
    /// not change its mind between two of them. See
    /// [`SelectionDraft::combining`].
    pub fn selection_press(&mut self, doc: Vec2, op: SelectionOp) {
        // A screen distance, divided by the zoom. A fixed *document* distance
        // would be impossible to hit at 10% and impossible to avoid at 800%.
        let close = SELECT_CLOSE_PIXELS / self.camera.zoom.max(1e-3);
        match self.selection_draft.as_mut() {
            Some(draft) => {
                if draft.press(doc, close) {
                    self.finish_selection();
                }
            }
            None => {
                self.selection_draft =
                    Some(SelectionDraft::new(self.ui.selection_mode, doc).combining(op));
                self.interaction = Interaction::Selecting;
            }
        }
    }

    pub fn selection_moved(&mut self, doc: Vec2) {
        if let Some(draft) = self.selection_draft.as_mut() {
            draft.moved(doc);
        }
    }

    pub fn selection_release(&mut self, doc: Vec2) {
        let Some(draft) = self.selection_draft.as_mut() else {
            // The draft went while the button was down — Escape, or a tool
            // shortcut. The button coming up is then what ends the gesture,
            // and leaving the interaction in `Selecting` would leave it with
            // nothing that could ever end it.
            self.interaction = Interaction::Idle;
            return;
        };
        if draft.release(doc) {
            self.finish_selection();
        }
    }

    /// Close the outline being drawn and combine it with whatever was already
    /// selected.
    ///
    /// A *plain* gesture that encloses nothing **clears** the selection rather
    /// than leaving the previous one standing. A bare click on the canvas is
    /// how every paint application spells "deselect", and keeping the old one
    /// would look like the tool had stopped answering. What an empty add or
    /// subtract does instead is [`Selection::combined`]'s to say.
    pub fn finish_selection(&mut self) {
        let Some(draft) = self.selection_draft.take() else {
            return;
        };
        self.interaction = Interaction::Idle;
        let shape = draft.finish(self.doc.size);
        self.selection =
            Selection::combined(self.selection.as_deref(), shape, draft.op()).map(Arc::new);
    }

    /// Abandon the outline being drawn, keeping whatever was selected before
    /// it started. Returns whether there was one — Escape does other things
    /// when there is not.
    pub fn cancel_selection_draft(&mut self) -> bool {
        let had = self.selection_draft.take().is_some();
        if had {
            self.interaction = Interaction::Idle;
        }
        had
    }

    /// Select the whole document again, which is what having no selection is.
    pub fn deselect(&mut self) {
        self.selection = None;
        self.cancel_selection_draft();
    }

    // --- floating transforms ------------------------------------------------

    /// How near a handle a press has to land, in document pixels.
    pub fn handle_tolerance(&self) -> f32 {
        HANDLE_GRAB_PIXELS / self.camera.zoom.max(1e-3)
    }

    /// The rectangle a transform would pick up: the selection, or the whole
    /// canvas where there is none.
    ///
    /// The whole canvas rather than the layer's own ink, because the engine
    /// does not know where a layer's ink is — finding out means reading it
    /// back, which blocks — and a transform of an empty region is harmless.
    pub fn transform_region(&self) -> umber_core::PixelRect {
        match self.selection.as_ref() {
            Some(sel) => sel.bounds(),
            None => umber_core::PixelRect {
                x: 0,
                y: 0,
                width: self.doc.size.x,
                height: self.doc.size.y,
            },
        }
    }

    /// Would a press here pick something up?
    ///
    /// Inside the selection, or anywhere on the canvas where there is none.
    /// Answered from the **outline** rather than from its bounding rectangle,
    /// which is what makes pressing beside a lasso mean "not this" instead of
    /// lifting the whole box the lasso happens to fit in.
    pub fn transform_would_grab(&self, doc: Vec2) -> bool {
        match self.selection.as_ref() {
            Some(sel) => sel.contains(doc),
            None => {
                let size = self.doc.size_vec2();
                doc.x >= 0.0 && doc.y >= 0.0 && doc.x < size.x && doc.y < size.y
            }
        }
    }

    /// A press on the canvas with the transform tool in hand, in document
    /// space. Returns what it took hold of, or `None` if there is no float.
    ///
    /// Only ever called with a float already up: picking one up needs the GPU,
    /// so `app.rs` does that first.
    ///
    /// **A press always takes hold of something now.** `Transform::grab` reads
    /// everywhere outside the box as a rotation, so the `None` that used to
    /// mean "put it down" is gone from here; deciding whether an outside press
    /// was a click or the start of a turn is `app.rs`'s, because it is a
    /// question about travel rather than about geometry.
    pub fn transform_press(&mut self, doc: Vec2) -> Option<Handle> {
        let tolerance = self.handle_tolerance();
        let float = self.float.as_mut()?;
        let handle = float.xf.grab(doc, tolerance);
        float.drag = Some((handle, doc));
        Some(handle)
    }

    /// The pointer moved with a handle held. `uniform` is Shift.
    pub fn transform_moved(&mut self, doc: Vec2, uniform: bool) -> bool {
        let Some(float) = self.float.as_mut() else {
            return false;
        };
        let Some((handle, from)) = float.drag else {
            return false;
        };
        float.xf.drag(handle, from, doc, uniform);
        // A move accumulates — it has nothing to be absolute against — so its
        // origin walks with the pointer. Every other handle is absolute against
        // where it was grabbed, which is what makes coming back to that point
        // come back to the transform it started with.
        if handle == Handle::Move {
            float.drag = Some((handle, doc));
        }
        true
    }

    pub fn transform_release(&mut self) {
        if let Some(float) = self.float.as_mut() {
            float.drag = None;
        }
    }

    /// Move the selection outline along with the pixels it described.
    ///
    /// Called at commit. Without it the marquee stays where the artist dragged
    /// the picture *from*, which then clips the next stroke to a region that no
    /// longer holds anything — an outline that lies about what it covers.
    ///
    /// The rings are geometry, so this is the forward transform applied to
    /// them and a re-rasterisation. Nothing in `selection.rs` needed changing
    /// for it.
    pub fn carry_selection(&mut self, xf: &Transform) {
        let Some(selection) = self.selection.as_ref() else {
            return;
        };
        let m = xf.matrix();
        let rings: Vec<Vec<Vec2>> = selection
            .rings()
            .iter()
            .map(|ring| ring.iter().map(|p| m.apply(*p)).collect())
            .collect();
        self.selection = Selection::from_rings(rings, self.doc.size).map(Arc::new);
    }

    /// Flatten the layer stack into what the composite pass consumes.
    ///
    /// Bottom-to-top, matching the shader's iteration order.
    ///
    /// `float` is `CanvasRenderer::float_preview`'s answer — the layer slot a
    /// floating transform stands in front of, and the slice holding the preview
    /// of it. Swapping the slot here is the **whole** of how a float reaches the
    /// screen: the preview slice already holds what the layer will hold once the
    /// pixels are put down, so the composite shader draws it at the right
    /// position, under the right blend mode, at the right opacity, without
    /// knowing a transform exists. See `CanvasRenderer::float_preview`.
    pub fn layer_draws(&self, float: Option<(u32, u32)>) -> Vec<LayerDraw> {
        self.layers
            .layers()
            .iter()
            .map(|l| LayerDraw {
                slot: match float {
                    Some((from, to)) if from == l.slot() => to,
                    _ => l.slot(),
                },
                opacity: l.opacity,
                blend: l.blend.index(),
                visible: l.visible,
            })
            .collect()
    }

    pub fn record_frame_time(&mut self, dt: f32) {
        self.frame_times[self.frame_cursor] = dt;
        self.frame_cursor = (self.frame_cursor + 1) % self.frame_times.len();
    }

    pub fn average_fps(&self) -> f32 {
        let sum: f32 = self.frame_times.iter().sum();
        let n = self.frame_times.iter().filter(|t| **t > 0.0).count();
        if n == 0 || sum <= 0.0 {
            0.0
        } else {
            n as f32 / sum
        }
    }

    /// True when pressure will be a flat 1.0, which is worth telling the user
    /// about rather than leaving them wondering why the pen feels dead.
    pub fn pressure_is_flat(&self) -> bool {
        matches!(self.pressure.source, PressureSource::Constant)
    }
}
