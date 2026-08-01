//! Editor state — everything that is not a GPU resource or a window.

use crate::colorpicker::{PickerMode, WheelShape};
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
    Brush, BrushMode, BrushPreset, Camera, Color, Document, History, Hsv, InputPoint, LayerStack,
    StrokeBuilder, TipMask,
    input::{PressureModel, PressureSource},
};
use umber_render::{LayerDraw, StrokeStyle};

/// What the pointer is currently doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interaction {
    Idle,
    Drawing,
    Panning,
    Zooming,
}

/// The selected tool. Brush and eraser paint; pan and zoom navigate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Brush,
    Eraser,
    Pan,
    Zoom,
}

impl Tool {
    pub fn paints(self) -> bool {
        matches!(self, Self::Brush | Self::Eraser)
    }
}

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
    pub picker: PickerMode,
    pub wheel_shape: WheelShape,
    /// Whether the wheel's triangle turns to follow the hue. Meaningless while
    /// the centre is the square, which has no orientation.
    pub wheel_rotates: bool,
    /// Open state of the picker-mode dropdown in the Colour panel header.
    pub picker_menu_open: bool,
    pub brush_editor_open: bool,
    pub brush_tab: BrushTab,
    pub settings_open: bool,
    pub settings_tab: SettingsTab,
    /// Tab whose close is waiting on confirmation, if any.
    pub close_prompt: Option<usize>,
    /// Which row of the brush editor's Inputs list is open for editing.
    ///
    /// An index rather than a copy of the entry, because the list is short and
    /// the entry is the brush's — a copy would need writing back and would go
    /// stale the moment a row above it was deleted.
    pub modulation: usize,
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
            picker: PickerMode::Wheel,
            wheel_shape: WheelShape::Triangle,
            // What the picker has always done, and what the design draws.
            wheel_rotates: true,
            picker_menu_open: false,
            brush_editor_open: false,
            brush_tab: BrushTab::Tip,
            settings_open: false,
            settings_tab: SettingsTab::Themes,
            close_prompt: None,
            modulation: 0,
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
    /// egui points per physical pixel, from the last frame. Window events
    /// arrive in physical pixels and the layout works in points, so hit-testing
    /// a cursor position against a floating panel needs the conversion.
    pub pixels_per_point: f32,

    pub stroke: StrokeBuilder,
    pub history: History,
    pub pressure: PressureModel,

    pub interaction: Interaction,
    /// Cursor in physical window pixels.
    pub cursor: Vec2,
    pub last_cursor: Vec2,
    /// Space held — temporary pan modifier.
    pub space_down: bool,
    /// Where a zoom-tool drag started; zooming keeps this point pinned.
    pub zoom_anchor: Vec2,

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
            // Read here rather than in `app.rs` so the window-creation path,
            // which several things already contend over, stays untouched.
            layout: Layout::load_or_default(),
            canvas_pivot: Vec2::ZERO,
            canvas_size: Vec2::ONE,
            pixels_per_point: 1.0,
            stroke: StrokeBuilder::new(),
            history: History::default(),
            pressure: PressureModel::default(),
            interaction: Interaction::Idle,
            cursor: Vec2::ZERO,
            last_cursor: Vec2::ZERO,
            space_down: false,
            zoom_anchor: Vec2::ZERO,
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

        InputPoint::new(doc, pressure, now)
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
        inside && !self.layout_owns_pointer(screen)
    }

    /// Select a tool, keeping the brush's paint/erase mode in step.
    pub fn set_tool(&mut self, tool: Tool) {
        self.ui.tool = tool;
        match tool {
            Tool::Brush => self.brush.mode = BrushMode::Paint,
            Tool::Eraser => self.brush.mode = BrushMode::Erase,
            Tool::Pan | Tool::Zoom => {}
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
        }
    }

    fn install_document(&mut self, state: DocumentState) {
        self.doc = state.doc;
        self.layers = state.layers;
        self.history = state.history;
        self.camera = state.camera;
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
            // Keep the zoom, but not the ability to be looking at a part of the
            // canvas that no longer exists.
            self.camera.center = self.camera.center.clamp(Vec2::ZERO, doc.size_vec2());
        }
        resized
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

    /// Flatten the layer stack into what the composite pass consumes.
    ///
    /// Bottom-to-top, matching the shader's iteration order.
    pub fn layer_draws(&self) -> Vec<LayerDraw> {
        self.layers
            .layers()
            .iter()
            .map(|l| LayerDraw {
                slot: l.slot(),
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
