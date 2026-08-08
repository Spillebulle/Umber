//! Editor state — everything that is not a GPU resource or a window.

use crate::colorpicker::{PickerMode, WheelAngles, WheelShape};
use crate::dock::Layout;
use crate::session::{DocId, DocumentState, Session};
use crate::settings::SettingsTab;
use crate::tabs::Notice;
use crate::theme::{Accent, Palette, ThemeKind};
use glam::Vec2;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use umber_core::{
    BlendMode, Brush, BrushMode, BrushPreset, Camera, Clip, Color, Document, EditTarget, Handle,
    Harmony, History, Hsv, InputPoint, LayerStack, Selection, SelectionDraft, SelectionMode,
    SelectionOp, StrokeBuilder, TipMask, Transform,
    input::{PressureModel, PressureSource},
};
use umber_render::{LayerDraw, LayerEffects, StrokeStyle};

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
    /// What a gesture does to the selection already standing, when no modifier
    /// says otherwise.
    ///
    /// A setting as well as a pair of modifiers because a held key is not
    /// discoverable and cannot be listed — see `App::selection_op`, which is
    /// where the two meet.
    pub selection_op: SelectionOp,
    /// How far a new selection's edge is softened, in document pixels.
    ///
    /// Interface state and not the document's: it describes what the *next*
    /// gesture will draw, exactly as the mode above does, and the radius a
    /// selection actually carries lives on the `Selection` itself.
    pub selection_feather: f32,
    pub picker: PickerMode,
    pub wheel_shape: WheelShape,
    /// Whether the wheel's triangle turns to follow the hue. Meaningless for the
    /// square, which has no corner that is the hue to keep beside the marker.
    pub wheel_rotates: bool,
    /// How far each wheel centre is turned from its neutral pose, when the hue
    /// is not deciding it. One angle per shape — see [`WheelAngles`].
    pub wheel_angles: WheelAngles,
    /// Which relation the Harmony picker mode shows. Meaningless in the other
    /// three, and kept anyway for the reason `wheel_shape` is: coming back to
    /// the mode should find the choice where it was left.
    pub harmony: Harmony,
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
/// `Hash` so it can salt the brush editor's scroll area: the position is per
/// section, or Inputs' offset would be carried onto Tip, which is short enough
/// to be left showing nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
            selection_op: SelectionOp::Replace,
            selection_feather: 0.0,
            picker: PickerMode::Wheel,
            wheel_shape: WheelShape::Triangle,
            // What the picker has always done, and what the design draws.
            wheel_rotates: true,
            // Zero is the pose every build before the angle existed drew.
            wheel_angles: WheelAngles::default(),
            harmony: Harmony::default(),
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
    /// The *name* [`Editor::tip`] was resolved from, which is what a save
    /// writes back onto `BrushPreset::tip`.
    ///
    /// Kept beside the mask rather than derived from it, because the two can
    /// legitimately disagree in both directions and each is a real state:
    ///
    /// - a **name with no mask** is a library copied without its `tips/`
    ///   directory. The brush paints round, as `BrushPreset::tip` promises, and
    ///   the reference has to survive an Update rather than being thrown away
    ///   on the machine that happens to be missing the picture.
    /// - a **mask with no name** is one just imported or drawn, which has not
    ///   been written into the library yet. Saving is what stores it and gives
    ///   it a name, through `UserLibrary::save`'s own `tips/` path.
    pub tip_name: Option<String>,
    /// Every mask the user's library holds, by name — what [`Editor::tip`] is
    /// resolved against. Filled in by `brushlib::resync`, which is also what
    /// keeps `presets` in step.
    pub tips: BTreeMap<String, Arc<TipMask>>,
    /// The *name* of the paper the brush in hand bites through, or `None` for
    /// whichever of the shipped three `Brush::grain_pattern` names.
    ///
    /// **A name and no mask beside it, which is deliberately not the tip's
    /// shape.** A tip can be drawn or imported into the hand and stay there,
    /// unnamed, until the brush is saved, so `Editor::tip` has to carry a mask
    /// the library has never seen. A paper cannot be in that state: it is a
    /// tile of the *document*, shared by construction, so it goes into the
    /// library the moment it arrives and is only ever chosen by name after
    /// that. One field and one resolver — [`Editor::paper_tile`] — is therefore
    /// the whole of it, where two would be two things to keep in step.
    pub paper_name: Option<String>,
    /// Every paper tile the user's library holds, by name. Filled in by
    /// `brushlib::resync` beside [`Editor::tips`].
    pub papers: BTreeMap<String, Arc<TipMask>>,
    pub layers: LayerStack,
    /// The layer list's thumbnails.
    ///
    /// Above the `--- documents ---` line because it names the document it
    /// belongs to and empties itself when that changes — the same arrangement
    /// the autosave's map of open documents keeps, and for the same reason: it
    /// is a cache of GPU state rather than part of the document. See
    /// [`crate::thumbs`].
    pub thumbs: crate::thumbs::Thumbs,
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
    /// Whether a stroke lands in the active layer or in its mask.
    ///
    /// Per-document, so it lives in [`DocumentState`] as well as here. Read
    /// through [`Editor::stroke_target`], never off the field: a target of
    /// `Mask` on a layer that has none is not a state anything downstream
    /// should have to consider.
    pub edit_target: EditTarget,
    /// Every open document, and which of them the fields above belong to.
    pub session: Session,
    /// A message that has to reach the user rather than the log — an import
    /// that could not be represented in full, or one that failed outright.
    pub notice: Option<Notice>,
    pub ui: UiState,
    /// The theme somebody made, when one is in use, or `None` for whichever of
    /// the two built-in themes [`UiState::theme`] names.
    ///
    /// Kept out of [`UiState`] so that stays `Copy` — it carries an id and a
    /// name, which are `String`s — and held here rather than looked up in the
    /// library every frame, because the palette is read on the drawing path and
    /// the library is a directory on disk. `UiState::theme` still holds the
    /// built-in this was made from, which is what [`Editor::palette`] falls
    /// back to when the file has gone.
    pub custom_theme: Option<crate::themelib::CustomTheme>,
    /// State of the New document and Canvas settings dialogs. Kept out of
    /// [`UiState`] so that stays `Copy` — it holds a colour picker's HSV, which
    /// has the same reason to be its own source of truth here as in the Colour
    /// panel. Seeded from the live document when a dialog opens.
    pub canvas_form: crate::canvasdlg::CanvasForm,
    /// State of the Export dialog, kept out of [`UiState`] for the reason
    /// `canvas_form` is — it holds a colour picker's HSV, for the matte. Not
    /// seeded per document and not cleared when one closes: a format and a
    /// quality are a way of working rather than a property of the picture.
    pub export_form: crate::exportdlg::ExportForm,
    /// The update check: whether it runs, what it last said, and how this copy
    /// was installed. Kept out of [`UiState`] because it holds a channel and a
    /// downloaded release, neither of which is `Copy`.
    pub updates: crate::update::Updates,
    /// The autosave: its schedule, the capture in flight and the thread that
    /// writes. Out of [`UiState`] for the same reason `updates` is — it holds
    /// channels and a map of every open document.
    pub autosave: crate::autosave::Autosave,
    /// What a session that did not end cleanly left behind, and what has been
    /// done about it. Out of [`UiState`] for the reason `autosave` is — it
    /// holds a list — and above the `--- documents ---` line because it
    /// describes a session that is over rather than any document that is open.
    pub recovery: crate::recoverdlg::Recovery,
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
    /// then vertical, `None` only where there is no document on that axis or
    /// the region is too short to hold a thumb.
    ///
    /// Recorded because a press on a bar must not also start a stroke, and the
    /// usual test cannot answer it: these sit *inside* the canvas region and are
    /// drawn in egui's background layer, so neither `pointer_over_canvas` nor
    /// the `layer_id_at` check in `app.rs` sees them. Set every frame by
    /// `ui::draw`, and an array rather than a `Vec` because it is written on the
    /// drawing path.
    ///
    /// This is load-bearing on every frame rather than only where the picture
    /// runs off the view: the bars are drawn whenever there is a document to
    /// move, so they are a permanent live target over two edges of the canvas.
    /// `ScrollSpan`'s docs have why they are drawn there.
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
    /// The live selection's three buttons — Deselect, Copy, Cut — as they were
    /// last drawn, in points, and `None` where the strip is not offered.
    ///
    /// Recorded for exactly the reason [`Editor::transform_buttons`] is, and it
    /// is the same problem with a worse failure: these sit *inside* the canvas
    /// region in egui's background layer, so without this a press on one would
    /// also be a press on the canvas — which with a brush in hand means a dab
    /// painted under the button that was clicked, inside the very selection the
    /// artist was about to copy.
    pub selection_buttons: [Option<egui::Rect>; 3],
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
    ///
    /// This is *Umber's* clipboard, and the desktop has one too. Which of the
    /// two a paste takes its picture off is `sysclip::decide`'s, and a picture
    /// off the desktop is adopted here on the way past — so a second Ctrl+V
    /// puts down the same picture once the desktop's clipboard has moved on.
    /// The board itself lives on `UmberApp`, not here: it is a resource the
    /// process holds rather than anything a session is.
    ///
    /// **It is never released, including a picture adopted off the desktop, and
    /// that was considered rather than overlooked.** There is no moment to hang
    /// a release on: [`Session::successor_of`] refuses to close the last tab —
    /// which is why the last document's tab draws no close mark — so "the last
    /// document went" is not a state Umber has. And a *foreign* clip is
    /// precisely the one whose memory releasing would not reclaim, because the
    /// desktop is still holding its own copy; all it would buy is a re-read on
    /// the next paste, at the cost of two lifetimes for one clipboard. A cap on
    /// the size is a separate question and is not answered here.
    ///
    /// **A second picture can be live beside this one**, and only one:
    /// `sysclip::OnDesktop::Echo`, on a platform whose clipboard does not hand
    /// back what it was given. `put_image` clears it before every write and
    /// `note_adopted` replaces it, so there is never more than one — and
    /// Windows and Linux keep none at all, because an echo equal to the clip is
    /// dropped. Where one is kept the worst case is worth naming: a bare Ctrl+X
    /// on the 10000² canvas the Undo section uses as its bound holds the
    /// 400 MB undo patch, 400 MB here, 400 MB of echo, and whatever the
    /// platform's own pasteboard keeps, with the decode buffer transiently on
    /// top of that.
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
    /// Written by [`Editor::note_input`] and [`Editor::sample`] from the event
    /// path, and by `InputLog::note_cursor` from `app::render` — the one entry
    /// that describes a *frame* rather than an event. Read only by the settings
    /// pane. Nothing on the stroke path may start reading it, or the diagnostic
    /// becomes part of what it is meant to be observing.
    pub input: crate::inputlog::InputLog,
    /// What the Text module holds: the block being composed, the face it is in,
    /// and the machine's fonts once they have been found.
    ///
    /// Above the `--- documents ---` line, like the clipboard and the brush in
    /// hand and for the same reason — a caption somebody is composing belongs
    /// to the person, not to one picture, and placing the same one on a second
    /// document is the ordinary thing to want. The font library is a *cache of
    /// the machine* rather than of a document, so a tab switch has nothing to
    /// do with it either.
    pub text: crate::textpanel::TextState,
    /// A directory of the user's own fonts, scanned beside the machine's.
    ///
    /// A *preference*, and here rather than in [`UiState`] because that struct
    /// is `Copy` and a path is not — which is also why it is not simply another
    /// `bool` beside `save_history`. Umber reads this folder and **copies
    /// nothing out of it**: the moment it copied a face it would be
    /// redistributing one, inside somebody's own documents folder, and
    /// `umber_core::fonts` says why that is the line. Changing it forgets the
    /// scan, because a library still holding the old folder's faces would offer
    /// faces the artist has just pointed Umber away from.
    pub font_folder: Option<std::path::PathBuf>,

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
    /// Whether the stroke that began should stamp its tip's **own colour**.
    ///
    /// The other half of the same snapshot, and it has to be its own field
    /// rather than being read back out of `stroke_style`: `per_dab_color` is
    /// true for a smudging brush too, so it cannot say whether the *tip's*
    /// colour was the thing that was wanted. `CanvasRenderer::set_tip` is
    /// handed this, so the dab pass and the pipeline choice are one decision —
    /// see `begin_stroke`, where both are made.
    pub stroke_stamps_colour: bool,
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
            tip_name: None,
            tips: BTreeMap::new(),
            paper_name: None,
            papers: BTreeMap::new(),
            layers: LayerStack::new(),
            thumbs: crate::thumbs::Thumbs::default(),
            session: Session::default(),
            notice: None,
            ui: UiState::default(),
            custom_theme: None,
            canvas_form: crate::canvasdlg::CanvasForm::default(),
            export_form: crate::exportdlg::ExportForm::default(),
            updates: crate::update::Updates::default(),
            autosave: crate::autosave::Autosave::default(),
            recovery: crate::recoverdlg::Recovery::default(),
            quit_requested: false,
            // Read here rather than in `app.rs` so the window-creation path,
            // which several things already contend over, stays untouched.
            layout: Layout::load_or_default(),
            canvas_pivot: Vec2::ZERO,
            canvas_size: Vec2::ONE,
            scroll_bars: [None, None],
            transform_buttons: [None, None],
            selection_buttons: [None, None, None],
            pixels_per_point: 1.0,
            selection: None,
            edit_target: EditTarget::Layer,
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
            text: crate::textpanel::TextState::default(),
            font_folder: None,
            interaction: Interaction::Idle,
            cursor: Vec2::ZERO,
            last_cursor: Vec2::ZERO,
            pen_pointer: false,
            space_down: false,
            zoom_anchor: Vec2::ZERO,
            brush_resize: None,
            stroke_style: StrokeStyle::default(),
            stroke_stamps_colour: false,
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

    /// The colours the interface is drawn in.
    ///
    /// One door, so that "which theme is in use" is answered in one place
    /// rather than at each of the dozen call sites that used to spell out
    /// `Palette::with_accent(ed.ui.theme, ed.ui.accent)` — and so that a theme
    /// somebody made needs no branch at any of them.
    ///
    /// A custom theme carries its accent as two of its own tokens, so
    /// [`UiState::accent`] does not re-hue it: the four-accent chooser is a
    /// shortcut for re-hueing a *built-in*, and applying it on top of colours
    /// somebody chose by hand would overwrite one of the things they chose. The
    /// pane draws that chooser only for a built-in, for the same reason.
    pub fn palette(&self) -> Palette {
        match &self.custom_theme {
            Some(theme) => theme.palette,
            None => Palette::with_accent(self.ui.theme, self.ui.accent),
        }
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
    /// sit *inside* the canvas region — the scrollbars, the floating
    /// transform's flip buttons and the selection's own strip — and are
    /// therefore not covered by [`Editor::layout_owns_pointer`].
    ///
    /// Every canvas overlay has to be listed here, and this is the *only* place
    /// they are: both the mouse's press path and the pen's reach it through
    /// [`Editor::pointer_over_canvas`], so a control added to one and not the
    /// other is the bug where a button works with a mouse and paints under a
    /// pen.
    pub fn canvas_overlay_owns_pointer(&self, screen: Vec2) -> bool {
        let at = self.to_points(screen);
        self.scroll_bars
            .iter()
            .chain(self.transform_buttons.iter())
            .chain(self.selection_buttons.iter())
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

    /// Where the canvas should draw its own pointer this frame, in physical
    /// window pixels — and `None` where the cursor the rest of the desktop gave
    /// us is the right one.
    ///
    /// The whole of the rule, in one place and as a pure function of state and
    /// one injected reading, so that it can be checked without a window. That
    /// matters more here than almost anywhere else in this crate: nobody
    /// working on Umber has a pen, so the only evidence this decision is right
    /// is a test that puts an [`Editor`] into the state a hovering pen leaves
    /// it in and asks. Injected rather than fetched for the reason
    /// `install::detect` takes a `Probe` and `keylayout::name_for` takes a
    /// reading — an `egui::Context` is exactly the thing a test has not got.
    ///
    /// It is **not** a test of the platform half. `syscursor` could be deleted
    /// and every test in this file would still pass — what they pin is that
    /// Umber *asks*, which is the half that can be reasoned about here. Whether
    /// the ask is carried out is a property of Windows and is settled from
    /// Settings → Input & pen on a machine with a tablet.
    ///
    /// Four things, and each was somewhere a pen could have been lost:
    ///
    /// - [`Editor::pen_pointer`] — *what kind* of device is driving the
    ///   pointer, latched from the last pointer event rather than preferred.
    ///   A finger counts, and deliberately: an arrow under a fingertip says as
    ///   little about where the mark will start as an arrow under a nib does.
    /// - [`Editor::pointer_over_canvas`] of [`Editor::cursor`] — over a panel
    ///   or one of the canvas's own overlay controls the ordinary cursor is the
    ///   right one, because those are things to point *at*. `cursor` is only
    ///   trustworthy here because a pen's hover writes it: winit gives a pen no
    ///   `CursorMoved`, so a build in which the hover branch merely recorded
    ///   the position and returned would be testing wherever the mouse was last
    ///   left — `(0, 0)` on a fresh launch, which is the menu bar.
    /// - [`Surroundings::over_area`] — anything egui is drawing *over* the
    ///   canvas rather than beside it. A modal, a menu or a dropdown sits
    ///   inside the central panel's rect, so `pointer_over_canvas` says yes,
    ///   and `set_cursor_icon` is last-write-wins within a frame. `pen_cursor`
    ///   is drawn inside the `CentralPanel`, which every dialog and every menu
    ///   is drawn *before* — so without this a pen in Settings or the brush
    ///   library overwrote that dialog's own cursor with "none" and painted the
    ///   dot into the background layer *underneath* it. No pointer at all,
    ///   which is the exact failure this function's use of `CursorIcon::None`
    ///   over `set_cursor_visible` was chosen to avoid. (Three `panels::` calls
    ///   do run after it and two of them set `Grabbing`; they are live only
    ///   during a panel drag, which is not a moment a dot is wanted either.)
    ///   [`over_egui_area`] is the one statement of the reading, shared with
    ///   `app::ui_owns_pointer`, so what suppresses the dot and what refuses a
    ///   press cannot drift apart.
    /// - [`Surroundings::focused`] — and this one has to be *here*, in what the
    ///   interface asks for, rather than at the platform call. It was at the
    ///   call, and that reintroduced the very failure `CursorIcon::None` was
    ///   chosen over `set_cursor_visible` to avoid: Alt-Tab away by keyboard
    ///   with the pen hovering and the request was still "none", so egui-winit
    ///   deduped it against `current_cursor_icon`, never called `set_cursor`,
    ///   and the blank shape stayed in force across the whole desktop. Declined
    ///   here, egui asks for a real `CursorIcon` instead, the dedupe passes and
    ///   winit puts the arrow back on the spot. It also stops an unfocused
    ///   window drawing a dot on its canvas, which the platform-side guard did
    ///   not.
    pub fn pen_dot(&self, around: Surroundings) -> Option<Vec2> {
        (self.pen_pointer
            && around.focused
            && !around.over_area
            && self.pointer_over_canvas(self.cursor))
        .then_some(self.cursor)
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
        // The name is kept whether or not it resolved — see `Editor::tip_name`.
        // Dropping it here would mean that opening a brush on a machine without
        // its `tips/` directory and pressing Update took the reference off the
        // brush for every machine.
        self.tip_name = preset.tip.clone();
        // The paper is a name and stays one — see `Editor::paper_name`. It is
        // resolved at `paper_tile`, not here, because unlike the tip it has no
        // second state to be caught in.
        self.paper_name = preset.paper.clone();
        self.active_preset = Some(index);
    }

    /// The tile the brush in hand paints through, or `None` for no grain.
    ///
    /// The one place a paper is resolved, and the two-tier lookup is the tip's:
    /// the user's library first, then the tiles Umber ships. Both hand back an
    /// `Arc` that is stable for as long as it is reachable, which is what
    /// `CanvasRenderer::set_grain`'s identity check needs — so calling this
    /// once per stroke costs a map lookup and a pointer copy, and never an
    /// upload.
    ///
    /// A name that resolves to neither answers `None`, which paints **flat**.
    /// That is `BrushPreset::paper`'s promise and it is the exact identity the
    /// shader already pays for; falling back to a shipped tile would put a
    /// grain the author never chose into the mark, which is how a Clip Studio
    /// import came to paint at 78% of its own opacity.
    pub fn paper_tile(&self) -> Option<Arc<TipMask>> {
        match &self.paper_name {
            Some(name) => self
                .papers
                .get(name)
                .cloned()
                .or_else(|| umber_core::tip::pattern(name).cloned()),
            None => umber_core::tip::pattern(self.brush.grain_pattern.key()).cloned(),
        }
    }

    /// Choose the paper by name, or go back to the shipped set with `None`.
    pub fn set_paper(&mut self, name: Option<String>) {
        self.paper_name = name;
    }

    /// Put a bitmap tip in the brush's hand, by name where it has one.
    ///
    /// `name` is `None` for a mask that is not in the library yet — one just
    /// imported or drawn — which `UserLibrary::save` stores and names when the
    /// brush is saved.
    pub fn set_tip(&mut self, mask: Arc<TipMask>, name: Option<String>) {
        self.tip = Some(mask);
        self.tip_name = name;
    }

    /// Take the bitmap tip off the brush in hand, without touching the preset
    /// it came from. Saving is what makes that stick.
    pub fn clear_tip(&mut self) {
        self.tip = None;
        // Both halves, or an Update would put the reference straight back.
        self.tip_name = None;
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
            edit_target: std::mem::take(&mut self.edit_target),
        }
    }

    fn install_document(&mut self, state: DocumentState) {
        self.doc = state.doc;
        self.layers = state.layers;
        self.history = state.history;
        self.camera = state.camera;
        self.selection = state.selection;
        self.edit_target = state.edit_target;
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
        // A folder is selected in the incoming document, so there is no slot
        // to carry: left as it was, since nothing will read it until a stroke
        // begins and `begin_stroke` refuses a folder outright.
        self.stroke_slot = self.layers.active_slot().unwrap_or(self.stroke_slot);
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
    /// patch in it is a rectangle of the old canvas, so not one of them still
    /// names the pixels it was captured from.
    ///
    /// **This is the one clearing structural undo cannot fix**, and the reason
    /// is worth having here rather than being rediscovered: parking a slice
    /// keeps a patch valid because the slice still holds those pixels, and
    /// `CanvasRenderer::resize` reallocates the whole layer array — so there is
    /// nothing to park, and a crop destroys pixels outside the new canvas that
    /// only a full copy of the old document would hold. Clearing here also
    /// releases every slice a parked layer was holding, which is correct: those
    /// layers were of a canvas that no longer exists.
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
    /// the GPU work actually happened. What is here is everything on the document
    /// that carries a **direction or a position**: the selection, which is
    /// geometry, and every layer effect's lighting. `LayerStack::flip_text`
    /// belongs here too and nothing calls it yet.
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
        // A selection that mirrors to nothing cannot arise from one that had
        // area — a mirror preserves it, and `Selection::flipped` keeps the hard
        // mirror rather than let a wide feather dissolve the mirrored shape.
        // `flipped` is an `Option` because `from_rings` is, and dropping it is
        // the right answer if it ever does: an outline covering nothing is no
        // selection.
        self.selection = self
            .selection
            .as_deref()
            .and_then(|sel| sel.flipped(axis, doc))
            .map(Arc::new);
        // Every layer effect carries a *direction* — where the light is — and a
        // flip that mirrored the pixels and left that alone is a whole document's
        // shadows disagreeing with its forms. `LayerStack::flip_text` belongs on
        // the next line when somebody wires it: this is the one place a flip
        // reaches the model, and both are the same job.
        self.layers.flip_effects(axis);
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

    // --- what a stroke lands in ---------------------------------------------

    /// Where a stroke would go: the slice, and whether it is a mask.
    ///
    /// The **one** place [`Editor::edit_target`] is turned into something the
    /// engine acts on. Asking for the mask of a layer that has none is not an
    /// error and not a refusal — it paints the layer, because the alternative
    /// is a brush that silently does nothing and a switch that has to be kept
    /// in step with which layer is selected.
    /// `None` where a folder is selected — it holds neither pixels nor a mask,
    /// so there is nowhere for a stroke to land. Every route to a stroke passes
    /// [`Editor::begin_stroke`], which refuses on it at the same gate a lock is
    /// refused at, so this is the shape rather than a case anything downstream
    /// has to invent an answer for.
    pub fn stroke_target(&self) -> Option<(u32, bool)> {
        match (self.edit_target, self.layers.active_mask()) {
            (EditTarget::Mask, Some(slot)) => Some((slot, true)),
            _ => Some((self.layers.active_slot()?, false)),
        }
    }

    /// True when the interface should show the mask as the thing being painted.
    pub fn editing_mask(&self) -> bool {
        self.stroke_target().is_some_and(|(_, mask)| mask)
    }

    /// What a stroke on a mask puts down.
    ///
    /// A mask holds coverage, not colour, so the palette is read as a **grey**:
    /// black hides, white reveals, and everything between is a partial. The
    /// eraser reveals — it paints white — rather than scaling the slice's alpha
    /// down, because the composite reads the red channel and an eraser that
    /// moved only the alpha would appear to do nothing at all. Forcing the mode
    /// here, once, is what keeps that out of the shader: `commit.wgsl` and
    /// `composite.wgsl` both see an ordinary paint stroke in a grey.
    fn mask_paint(&self) -> (Color, BrushMode) {
        let level = match self.brush.mode {
            BrushMode::Erase => 1.0,
            BrushMode::Paint => self.color.luminance(),
        };
        (
            Color {
                r: level,
                g: level,
                b: level,
                a: 1.0,
            },
            BrushMode::Paint,
        )
    }

    /// Begin a stroke, unless the layer it would land on is locked.
    ///
    /// **The lock is refused here and nowhere else on the painting path.** Every
    /// route to a stroke — mouse, pen, touch, a shortcut that starts one —
    /// arrives at this function, so a check spread over those call sites is one
    /// that a later route would miss. Returns whether the stroke started, which
    /// is what tells the caller not to put the interaction into `Drawing`.
    pub fn begin_stroke(&mut self, point: InputPoint) -> bool {
        if self.layers.active_is_locked() {
            return false;
        }
        // A folder holds no pixels and no mask, so there is nowhere for the
        // stroke to go. Refused at this same one gate rather than at the four
        // routes that reach it, exactly as the lock above is — and silently,
        // because this is reached every time the pen goes down.
        let Some((slot, on_mask)) = self.stroke_target() else {
            return false;
        };
        let (color, mode) = if on_mask {
            self.mask_paint()
        } else {
            (self.color, self.brush.mode)
        };
        // Whether the tip in hand stamps a colour of its own.
        //
        // It cannot be read off `Brush`, for `Brush::dab_has_angle`'s reason:
        // `BrushPreset::tip` is a *name* and the editor is what resolves it. So
        // the two halves of "does this stroke carry a colour per dab" are
        // combined here, which is also the one place they are snapshotted.
        //
        // Refused where there is nowhere for a colour to land, at this one gate
        // rather than at the preview and the commit separately — the rule the
        // blend mode two lines below already follows. **An eraser** deposits no
        // colour, and **a mask** holds coverage on one channel, so a stamp's
        // reds and blues would become "reveal" and "hide". A coloured stamp used
        // for either paints as the mask it also is, which is what those two
        // tools mean by it, and costs no colour attachment at all.
        //
        // The answer goes to *both* halves — `per_dab_color` below and
        // `CanvasRenderer::set_tip`, through `Editor::stroke_stamps_colour`.
        // Refusing only the first was a real bug and a subtle one: a brush that
        // smudges **and** carries a coloured tip turns `per_dab_color` on for
        // its own reason, so the dab pass would still have stamped the tip's
        // colour into a mask, which previews grey and commits red.
        self.stroke_stamps_colour = mode == BrushMode::Paint
            && !on_mask
            && self.tip.as_ref().is_some_and(|tip| tip.is_coloured());
        // Snapshot the brush: the user can change colour, opacity or layer via
        // the panel mid-stroke, but the stroke must finish as it started.
        self.stroke_style = StrokeStyle {
            color,
            opacity: self.brush.opacity,
            mode,
            // Snapshotted like everything else here, and coerced at this one
            // gate rather than at the preview and the commit separately.
            //
            // Two things take it back to Normal. `effective_blend` refuses one
            // for an eraser, because a blend mode is a rule for combining a
            // colour and an eraser deposits none. And a stroke on a *mask* has
            // none either: a mask holds coverage on one channel, its preview is
            // a one-channel blend written to match what the commit puts in the
            // slice, and a second mode running through that pair would be a
            // second place for those two to disagree — for a control whose
            // meaning on a coverage channel nobody has defined.
            blend: if on_mask {
                BlendMode::Normal
            } else {
                self.brush.effective_blend()
            },
            // Decided once, here, from the brush this stroke started with. It
            // must not change mid-stroke: dabs already stamped without a colour
            // recorded would commit as the flat palette colour while the rest
            // smudged.
            //
            // Colour pickup is no longer the only thing that colours a dab: a
            // hue, saturation or brightness modulation does too, and so does a
            // **coloured stamp** — a tip whose texels carry colour rather than
            // coverage alone. All three write the one colour scratch, which is
            // per fragment already, so the third needed no attachment, no
            // pipeline and no line of `composite.wgsl` or `commit.wgsl`.
            //
            // This is the field `app.rs` builds its `DabStyle` from, so the two
            // cannot disagree about which pipeline the frame uses — the thing
            // that must hold for every frame of a stroke.
            per_dab_color: self.brush.colours_dabs() || self.stroke_stamps_colour,
            // Snapshotted with everything else, for the same reason: switching
            // the edit target mid-stroke must not send the second half of a
            // mark somewhere the first half did not go.
            on_mask,
        };
        // The mask's slice, when that is what is being painted. Captured here
        // exactly as the layer's was, so selecting another layer — or turning
        // the mask switch — mid-stroke cannot land the commit elsewhere.
        self.stroke_slot = slot;
        self.pressure.reset();
        // `Color` is already linear — the engine works in linear throughout —
        // so this is the same value the composite would have used. The
        // snapshotted colour rather than the palette, so a mask stroke's grey
        // is what the dabs carry too.
        let color = self.stroke_style.color;
        let paint = [color.r, color.g, color.b];
        let mut brush = self.brush;
        brush.mode = self.stroke_style.mode;
        self.stroke.begin(brush, paint, point);
        self.interaction = Interaction::Drawing;
        true
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
    /// [`SelectionDraft::combining`]. The feather is snapshotted there too, and
    /// is taken from the interface here rather than passed in because — unlike
    /// the operation — no modifier can change it and there is nothing to
    /// reconcile.
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
                self.selection_draft = Some(
                    SelectionDraft::new(self.ui.selection_mode, doc)
                        .combining(op)
                        .feathered(self.ui.selection_feather),
                );
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
        // A move and a rotation both accumulate — neither has anything to be
        // absolute against — so their origin walks with the pointer, and each
        // event applies only the distance or the angle since the last one. A
        // *scale* is absolute against the handle it grabbed, which is what
        // makes coming back to that point come back to the transform it
        // started with.
        //
        // Leaving the rotation pinned to the press was a real bug: the same
        // offset was added again on every event, so the box spun away from the
        // hand, always in the direction of the first flick.
        if matches!(handle, Handle::Move | Handle::Rotate) {
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
    /// them and a re-rasterisation.
    ///
    /// **The feather is re-applied**, for the reason `Selection::flipped` does
    /// it: the rebuilt mask is the *sharp* rasterisation of the moved rings, so
    /// dragging a softly selected region across the canvas would otherwise put
    /// it down with a hard edge. The radius is left as it was rather than
    /// scaled by the transform — a feather is a distance in document pixels,
    /// the same thing the control on the strip says, and a non-uniform scale
    /// has no single number to scale it by anyway.
    ///
    /// And it falls back to the hard mirror where the radius dissolves the
    /// moved shape, exactly as `Selection::flipped` does and for the reason
    /// stated there: scaling a small feathered region down is a way to reach
    /// that, and losing the marquee because a transform was committed is worse
    /// than carrying it hard.
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
        let feather = selection.feather();
        let doc = self.doc.size;
        self.selection = Selection::from_rings(rings, doc)
            .map(|sharp| sharp.clone().feathered(feather, doc).unwrap_or(sharp))
            .map(Arc::new);
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
    /// **Folders are flattened away here, and that is the whole of what a
    /// pass-through folder costs the renderer.**
    ///
    /// A folder holds no pixels and has no opacity or blend mode of its own, so
    /// its contents composited in place *are* the group — which is why
    /// `composite.wgsl` was not touched for folders, why the four other things
    /// that reuse that pass (`export_rgba`, `pick_colour`, `probe_canvas` and
    /// the autosave's capture) needed nothing, and why a document of folders
    /// still declares the same file-format revision. All a folder contributes
    /// is its eye, and that folds into its contents because visibility is a
    /// boolean: `hidden ∧ anything = hidden`. An *opacity* would not fold, and
    /// is exactly the thing that would need an accumulator stack in the shader —
    /// see `docs/layer-folders.md`.
    ///
    /// The consequence to hold on to is that a draw's position is **not** a
    /// stack position once a document has folders in it. Anything handing the
    /// composite a stack index has to map it through
    /// [`Editor::active_draw_index`].
    pub fn layer_draws(&self, float: Option<(u32, u32)>) -> Vec<LayerDraw> {
        self.effected_draws(float)
            .into_iter()
            .map(|e| e.draw)
            .collect()
    }

    /// The same flattening, with each layer's effects beside its draw.
    ///
    /// [`Editor::layer_draws`] is this with the effects thrown away, so the two
    /// cannot disagree about folders, about visibility or about which slice a
    /// floating transform substitutes — one rule, in one place, which is the
    /// arrangement the whole of that method's docs argue for.
    ///
    /// Nothing here decides which effects are *drawn*: that is
    /// `CanvasRenderer::bake_effects`', because it is the crate that knows how
    /// many effect slices there are and which of them are already baked. What is
    /// decided here is only which of them could be — an effect that would mark
    /// nothing is filtered out by `umber_render::effect_marks_nothing`, so the
    /// draw list and the bake read one rule rather than agreeing by discipline.
    ///
    /// A folder carries no effects: `LayerStack::set_effect` refuses one, because
    /// a folder holds no coverage to derive from until group compositing lands.
    /// So the `filter_map` below drops folders exactly as it always did and never
    /// has to ask.
    pub fn effected_draws(&self, float: Option<(u32, u32)>) -> Vec<LayerEffects<'_>> {
        self.layers
            .layers()
            .iter()
            .enumerate()
            .filter_map(|(i, l)| {
                Some(LayerEffects {
                    draw: LayerDraw {
                        slot: match (l.slot()?, float) {
                            (slot, Some((from, to))) if from == slot => to,
                            (slot, _) => slot,
                        },
                        opacity: l.opacity,
                        blend: l.blend.index(),
                        visible: self.layers.effective_visible(i),
                        // The mask is *not* swapped for the preview slice: a
                        // floating transform moves the layer's pixels, not what
                        // hides them, and the preview has to be masked exactly as
                        // the committed result will be.
                        mask: l.mask(),
                        clipped: l.clipped,
                    },
                    effects: l.effects(),
                })
            })
            .collect()
    }

    /// The lowest texture-array slice a baked effect may take.
    ///
    /// One past everything `LayerStack` has claimed, plus one for the slice a
    /// floating transform previews into — which `CanvasRenderer::begin_float`
    /// takes at exactly `slot_capacity_needed()`.
    ///
    /// **`slot_capacity_needed` and not the highest slot in the draw list**, and
    /// the difference is a document quietly damaged. A slice parked in an undo
    /// entry is claimed and is in no layer, so it appears in no draw; it is below
    /// this number because `SlotPool` compacts only its tail. An effect written
    /// at the top of the draw list instead would be an effect written over a
    /// deleted layer's pixels, discovered when somebody undid the delete.
    pub fn effect_slot_base(&self) -> u32 {
        self.layers.slot_capacity_needed() + 1
    }

    /// Where the selected layer sits in [`Editor::layer_draws`].
    ///
    /// The composite is told which draw carries the stroke in flight, and with
    /// folders in the stack that is no longer the stack position: every folder
    /// below the active layer shifts it by one. Getting this wrong previews the
    /// stroke on the wrong layer — under the wrong blend mode, at the wrong
    /// opacity — and then it jumps at pointer-up when the commit puts it where
    /// it really went.
    ///
    /// A folder cannot be painted on, so a folder selected answers with a
    /// position past the end, which the shader's `i == active_index` simply
    /// never matches.
    pub fn active_draw_index(&self) -> u32 {
        let active = self.layers.active_index();
        if self.layers.active_is_folder() {
            // Deliberately not "the layer below it". Counting the layers under
            // a folder and subtracting one lands on a real draw, and the stroke
            // preview would then appear on a layer the painter did not choose.
            return u32::MAX;
        }
        self.layers
            .layers()
            .iter()
            .take(active)
            .filter(|l| !l.is_folder())
            .count() as u32
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

/// What [`Editor::pen_dot`] needs to know about the window this frame and
/// cannot find out for itself.
///
/// A struct of injected readings rather than two loose `bool` arguments, for
/// the reason `install::detect` takes a `Probe`: the call site says which is
/// which, and a test can state the case it means without counting positions.
/// Both come from egui — one from the layer under the pointer, one from
/// `InputState::focused` — and neither is anything an `Editor` holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Surroundings {
    /// Is egui drawing a menu, a dropdown, a modal or a floating panel over
    /// the canvas at the pointer? See [`over_egui_area`].
    pub over_area: bool,
    /// Does this window have the keyboard focus? A cursor belongs to whoever
    /// the user is working in, so an unfocused Umber asks for nothing.
    pub focused: bool,
}

/// Is egui drawing something of its own *over* the canvas at `screen`?
///
/// A menu, a popup, a modal or a floating panel — all of them `Area`s, all of
/// them inside the central panel's rect and therefore invisible to
/// [`Editor::pointer_over_canvas`], which is derived from that rect.
///
/// One function because there are two things that must agree about it and they
/// are in different modules: `app::ui_owns_pointer` refuses a *press* there,
/// and [`Editor::pen_dot`] refuses the *dot* there. Two copies of the same
/// `layer_id_at` line is how a dialog ends up taking the press and losing the
/// pointer, or the reverse.
pub fn over_egui_area(editor: &Editor, ctx: &egui::Context, screen: Vec2) -> bool {
    ctx.layer_id_at(editor.to_points(screen))
        .is_some_and(|layer| layer.order != egui::Order::Background)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one place a paper name becomes a tile, and every state it can be in.
    ///
    /// The failure that matters is the last one: a name that resolves to
    /// nothing must paint **flat**, not fall back to a shipped tile. Grain
    /// multiplies coverage, so a substituted paper is a stroke weaker than its
    /// own opacity through pits its author never drew — which is exactly what
    /// the Clip Studio importer used to produce, at 78% of the opacity it was
    /// set to.
    #[test]
    fn a_paper_name_resolves_to_the_users_tile_then_umbers_then_to_nothing_at_all() {
        let mut ed = Editor::default();

        // No name: whichever of the shipped three the brush's own enum says,
        // which is what every brush written before papers had names does.
        ed.brush.grain_pattern = umber_core::GrainPattern::Grit;
        let shipped = ed.paper_tile().expect("a shipped tile");
        assert!(Arc::ptr_eq(
            &shipped,
            umber_core::tip::pattern("grit").expect("shipped")
        ));

        // A name Umber ships, which is how a preset can pin one whatever the
        // enum happens to hold.
        ed.set_paper(Some("canvas".to_owned()));
        assert!(Arc::ptr_eq(
            &ed.paper_tile().expect("a shipped tile"),
            umber_core::tip::pattern("canvas").expect("shipped")
        ));
        assert_eq!(
            ed.brush.grain_pattern,
            umber_core::GrainPattern::Grit,
            "the name overrides the enum rather than rewriting it"
        );

        // The user's library first, so a tile of theirs taking a shipped name
        // wins — `apply_preset`'s order for the tip, and the browser says which
        // is which rather than hiding one.
        let mine = Arc::new(TipMask::new(2, 2, vec![7; 4]).expect("tile"));
        ed.papers.insert("canvas".to_owned(), Arc::clone(&mine));
        assert!(Arc::ptr_eq(&ed.paper_tile().expect("mine"), &mine));

        // And a name behind nothing at all — a library copied without its
        // `papers/` directory.
        ed.set_paper(Some("gone".to_owned()));
        assert!(
            ed.paper_tile().is_none(),
            "an unresolvable paper must paint flat, not through a stranger's tile"
        );
    }

    /// Selecting a brush carries its paper, and carries the *absence* of one.
    /// A name left standing from the previous brush would be a paper on a brush
    /// whose author never asked for one.
    #[test]
    fn selecting_a_brush_takes_its_paper_and_drops_the_last_ones() {
        let mut ed = Editor::default();
        let papered = umber_core::BrushPreset {
            paper: Some("linen".to_owned()),
            ..umber_core::BrushPreset::fresh("Papered")
        };
        let plain = umber_core::BrushPreset::fresh("Plain");
        ed.presets = vec![papered, plain];

        ed.apply_preset(0);
        assert_eq!(ed.paper_name.as_deref(), Some("linen"));
        ed.apply_preset(1);
        assert!(ed.paper_name.is_none());
    }

    /// **Every canvas overlay has to be in `canvas_overlay_owns_pointer`**, and
    /// until this there was nothing that would notice one falling out of the
    /// chain. All three kinds are checked together — the scrollbars, the
    /// transform's flip pair and the selection's strip — because the failure is
    /// always the same and always silent: the control still draws, still hovers
    /// and still clicks, and the press *also* reaches the canvas underneath it.
    /// With a brush in hand that is a dab under the button that was pressed.
    ///
    /// This is the whole of what both press paths consult — the mouse's through
    /// `ui_owns_pointer` and the pen's through the same call in the `Touch`
    /// branch — so a hole here is a hole in both.
    #[test]
    fn every_canvas_overlay_takes_the_pointer_off_the_canvas() {
        // A canvas region filling a 1000 x 800 window, points and pixels alike.
        let region = |ed: &mut Editor| {
            ed.pixels_per_point = 1.0;
            ed.canvas_pivot = Vec2::new(500.0, 400.0);
            ed.canvas_size = Vec2::new(1000.0, 800.0);
        };
        let middle = Vec2::new(500.0, 400.0);
        let mut bare = Editor::default();
        region(&mut bare);
        assert!(
            bare.pointer_over_canvas(middle),
            "open canvas has to belong to the document"
        );

        let at = egui::Rect::from_min_size(egui::pos2(480.0, 380.0), egui::vec2(40.0, 40.0));
        for (name, place) in [
            ("a scrollbar", 0usize),
            ("a transform flip button", 1),
            ("a selection button", 2),
        ] {
            let mut ed = Editor::default();
            region(&mut ed);
            match place {
                0 => ed.scroll_bars[0] = Some(at),
                1 => ed.transform_buttons[1] = Some(at),
                _ => ed.selection_buttons[2] = Some(at),
            }
            assert!(
                ed.canvas_overlay_owns_pointer(middle),
                "{name} did not claim the pointer"
            );
            assert!(
                !ed.pointer_over_canvas(middle),
                "a press on {name} would also reach the canvas"
            );
            // And only where it actually is: an overlay must not take the whole
            // canvas with it.
            assert!(
                ed.pointer_over_canvas(Vec2::new(100.0, 100.0)),
                "{name} claimed canvas it does not cover"
            );
        }
    }

    /// An editor in a 1000 x 800 window, points and pixels alike, whose canvas
    /// region starts **below a 30 px menu bar** and which **no pointer event
    /// has touched** — so `cursor` is still where a fresh launch leaves it.
    ///
    /// The menu bar is what makes the fixture able to fail. With the canvas
    /// filling the window, `(0, 0)` is *over* the canvas, so the stale-`cursor`
    /// case the test below describes would answer `Some` and the test would
    /// pass while the bug it names was present. Here the origin is over the
    /// menu bar, exactly as it is in the running application.
    ///
    /// `layout` is set rather than inherited because `Editor::default` loads
    /// the *developer's own* saved workspace, and a floating panel parked over
    /// the middle of the window would fail these tests on one machine and pass
    /// on every other.
    ///
    /// A struct literal rather than `Editor::default()` followed by
    /// assignments, which says the same thing and is what clippy's
    /// `field_reassign_with_default` refuses.
    fn windowed() -> Editor {
        Editor {
            pixels_per_point: 1.0,
            canvas_pivot: Vec2::new(500.0, 415.0),
            canvas_size: Vec2::new(1000.0, 770.0),
            layout: Layout::default(),
            ..Editor::default()
        }
    }

    /// **A hovering pen must reach the dot with no mouse event behind it**,
    /// which is the case a machine with a tablet is in and the machine this was
    /// written on can never be.
    ///
    /// The trap is `Editor::cursor`. It is written by `CursorMoved`, and a pen
    /// on Windows Ink produces none — so if the hover branch of `window_event`
    /// ever goes back to recording the position and returning, `cursor` here
    /// holds wherever the mouse was last left. On a fresh launch that is
    /// `(0, 0)`, the corner of the menu bar, which is never over the canvas:
    /// `pen_dot` would answer `None` for the whole time the pen is hovering,
    /// which is exactly when the arrow is on screen and complained about. So
    /// this starts from an editor no pointer event has touched and writes
    /// *only* what a hover writes.
    ///
    /// It is also the guard on the second half of that bug: the dot is drawn at
    /// the position this answers with, so a stale `cursor` put the dot in the
    /// wrong place as well as suppressing it.
    /// The ordinary case: focused, with nothing of egui's over the pointer.
    fn clear() -> Surroundings {
        Surroundings {
            over_area: false,
            focused: true,
        }
    }

    #[test]
    fn a_hovering_pen_asks_for_its_own_cursor_with_no_mouse_event_behind_it() {
        let mut ed = windowed();
        assert_eq!(
            ed.cursor,
            Vec2::ZERO,
            "the point of this test is that no mouse event has moved it"
        );
        assert_eq!(
            ed.pen_dot(clear()),
            None,
            "a mouse keeps the desktop's own arrow"
        );

        // The regression this fixture exists to be able to catch: a pen is
        // driving, but the hover never wrote `cursor`. The origin is the menu
        // bar, so the answer must be "no dot" — and it must be that for the
        // *stated* reason, which the next line pins by construction.
        ed.pen_pointer = true;
        assert!(
            !ed.pointer_over_canvas(Vec2::ZERO),
            "the fixture has to put the origin off the canvas or it proves nothing"
        );
        assert_eq!(
            ed.pen_dot(clear()),
            None,
            "a stale cursor must not be read as a pen over the canvas"
        );

        // And what the `Contact::Hover` branch actually leaves behind: the kind
        // of device, and the position out of the touch's own event.
        let at = Vec2::new(620.0, 310.0);
        ed.cursor = at;
        assert_eq!(
            ed.pen_dot(clear()),
            Some(at),
            "a pen hovering over the canvas has to get the dot, at the nib"
        );
    }

    /// **A pen over a modal, a menu or a dropdown keeps the ordinary cursor.**
    ///
    /// Every dialog and every menu is drawn *before* the `CentralPanel` that
    /// `pen_cursor` sits in, and `Context::set_cursor_icon` is last-write-wins
    /// within a frame — so without this the dot won over the dialog's own
    /// cursor and was painted into the background layer *underneath* it. No
    /// pointer at all, which is the failure `CursorIcon::None` was chosen over
    /// `set_cursor_visible` to prevent. It cannot be caught by
    /// `pointer_over_canvas`: an egui `Area` claims no space, so the canvas
    /// rect is the same whether one is up or not — a rule this codebase relies
    /// on elsewhere and exactly what makes this case invisible.
    ///
    /// What this pins is the **rule**, with the reading passed in. Whether
    /// `pen_cursor` supplies the right reading is `over_egui_area`'s and cannot
    /// be checked without an `egui::Context` — worth knowing, because that
    /// function's answer under a modal is not the plain hit test it looks like:
    /// egui returns the modal's own layer for *every* point in the window. That
    /// is right for this rule and it is why `InputLog::note_cursor` has to skip
    /// those frames.
    #[test]
    fn a_pen_over_a_dialog_keeps_the_ordinary_cursor() {
        let mut ed = windowed();
        ed.pen_pointer = true;
        ed.cursor = Vec2::new(500.0, 415.0);

        assert!(
            ed.pen_dot(clear()).is_some(),
            "the same point with nothing over it is the dot's"
        );
        assert_eq!(
            ed.pen_dot(Surroundings {
                over_area: true,
                ..clear()
            }),
            None,
            "a pen over a modal must keep a pointer it can aim with"
        );
    }

    /// **An unfocused window asks for nothing**, and the reason this is a rule
    /// of the *request* rather than of the platform call is a bug it caused.
    ///
    /// Alt-Tab away by keyboard with a pen hovering over the canvas: no pointer
    /// event follows, so with focus tested only beside `SetCursor` the request
    /// stayed "none", egui-winit deduped it against `current_cursor_icon` and
    /// never called `set_cursor`, and the blank shape stayed in force over the
    /// whole desktop until the mouse crossed a window that set its own. That is
    /// precisely "a window with no pointer in it and no way to say so", which
    /// `pen_cursor`'s choice of `CursorIcon::None` exists to avoid.
    #[test]
    fn an_unfocused_window_puts_the_cursor_back() {
        let mut ed = windowed();
        ed.pen_pointer = true;
        ed.cursor = Vec2::new(500.0, 415.0);
        assert!(ed.pen_dot(clear()).is_some(), "focused, the dot is right");
        assert_eq!(
            ed.pen_dot(Surroundings {
                focused: false,
                ..clear()
            }),
            None,
            "an unfocused window must ask for a real cursor, not for none"
        );
    }

    /// Over a panel the ordinary cursor is the right one — those are things to
    /// point at — and over a canvas overlay too, for the same reason a press
    /// there is not a dab. Both halves through `pointer_over_canvas`, so this
    /// cannot start disagreeing with where a press goes.
    #[test]
    fn a_pen_over_something_to_point_at_keeps_the_ordinary_cursor() {
        let mut ed = windowed();
        ed.pen_pointer = true;

        ed.cursor = Vec2::new(500.0, 400.0);
        assert!(ed.pen_dot(clear()).is_some(), "open canvas is the dot's");

        // Outside the canvas region on the x axis. The fixture's canvas spans
        // the full width, so this is off the *window* rather than over a docked
        // panel — which is the same arithmetic `pointer_over_canvas` does for a
        // panel and is all this line claims.
        ed.cursor = Vec2::new(1400.0, 400.0);
        assert_eq!(
            ed.pen_dot(clear()),
            None,
            "a pen outside the canvas region keeps the arrow"
        );

        // A docked panel proper: inside the window, claimed by the layout.
        let mut docked = windowed();
        docked.pen_pointer = true;
        docked.canvas_size = Vec2::new(736.0, 770.0);
        docked.canvas_pivot = Vec2::new(368.0, 415.0);
        docked.cursor = Vec2::new(900.0, 400.0);
        assert_eq!(
            docked.pen_dot(clear()),
            None,
            "a pen over a docked panel keeps the arrow"
        );

        // Inside the region, over one of the canvas's own controls.
        ed.cursor = Vec2::new(500.0, 400.0);
        ed.selection_buttons[0] = Some(egui::Rect::from_min_size(
            egui::pos2(480.0, 380.0),
            egui::vec2(40.0, 40.0),
        ));
        assert_eq!(
            ed.pen_dot(clear()),
            None,
            "a pen over a canvas overlay keeps the arrow"
        );
    }

    fn point() -> InputPoint {
        InputPoint::new(Vec2::splat(10.0), 1.0, 0.0)
    }

    /// The gate every route to a stroke passes through. Checked here rather
    /// than at the four call sites in `app.rs` that reach it, which is the
    /// whole of how a lock cannot be forgotten by a fifth.
    #[test]
    fn a_locked_layer_refuses_a_stroke() {
        let mut ed = Editor::default();
        assert!(ed.begin_stroke(point()), "an unlocked layer paints");
        assert_eq!(ed.interaction, Interaction::Drawing);

        ed.stroke.end();
        ed.interaction = Interaction::Idle;
        ed.layers.active_mut().locked = true;
        assert!(!ed.begin_stroke(point()), "a locked layer must refuse");
        assert_eq!(
            ed.interaction,
            Interaction::Idle,
            "a refused stroke must not leave the pointer drawing"
        );
        assert!(!ed.stroke.is_active());
    }

    /// The edit target only means anything on a layer that has a mask, and
    /// [`Editor::stroke_target`] is the one place that decides it — so nothing
    /// downstream ever sees "paint the mask" on a layer with none.
    #[test]
    fn the_edit_target_falls_back_to_the_layer_without_a_mask() {
        let mut ed = Editor {
            edit_target: EditTarget::Mask,
            ..Default::default()
        };
        assert_eq!(
            ed.stroke_target(),
            ed.layers.active_slot().map(|s| (s, false))
        );
        assert!(!ed.editing_mask());

        let mask = ed.layers.add_mask(0).unwrap();
        assert_eq!(ed.stroke_target(), Some((mask, true)));
        assert!(ed.editing_mask());
    }

    /// A mask holds coverage, so a stroke on one is a grey — and the eraser
    /// reveals rather than scaling the slice's alpha down, which the composite
    /// would not see at all. Both are decided once, in `begin_stroke`, and
    /// travel to the preview and the commit as one snapshotted `StrokeStyle`.
    #[test]
    fn a_stroke_on_a_mask_is_a_grey_and_the_eraser_reveals() {
        let mut ed = Editor::default();
        let mask = ed.layers.add_mask(0).unwrap();
        ed.edit_target = EditTarget::Mask;
        ed.set_color(Color::new(0.0, 1.0, 0.0, 1.0));

        assert!(ed.begin_stroke(point()));
        assert!(ed.stroke_style.on_mask);
        assert_eq!(ed.stroke_slot, mask, "the commit must land in the mask");
        let c = ed.stroke_style.color;
        assert_eq!((c.r, c.g, c.b), (c.r, c.r, c.r), "a mask stroke is a grey");
        assert!(c.r > 0.0 && c.r < 1.0, "green is a mid grey, not black");

        ed.interaction = Interaction::Idle;
        ed.brush.mode = BrushMode::Erase;
        assert!(ed.begin_stroke(point()));
        assert_eq!(ed.stroke_style.mode, BrushMode::Paint);
        assert_eq!(ed.stroke_style.color.r, 1.0, "the eraser reveals");
    }

    /// Painting the layer is untouched by any of the above: the same slot, the
    /// palette colour, and the mode the tool is in.
    #[test]
    fn painting_the_layer_is_exactly_what_it_was() {
        let mut ed = Editor::default();
        ed.layers.add_mask(0);
        ed.set_color(Color::new(0.25, 0.5, 0.75, 1.0));
        ed.brush.mode = BrushMode::Erase;

        assert!(ed.begin_stroke(point()));
        assert!(!ed.stroke_style.on_mask);
        assert_eq!(Some(ed.stroke_slot), ed.layers.active_slot());
        assert_eq!(ed.stroke_style.color, ed.color);
        assert_eq!(ed.stroke_style.mode, BrushMode::Erase);
    }

    /// A brush's blend mode is snapshotted with the rest of the stroke, and
    /// taken back to Normal at this one gate in the two cases where it means
    /// nothing — an eraser, which deposits no colour for a mode to combine, and
    /// a stroke on a mask, whose preview blends one coverage channel against a
    /// commit written to match it.
    ///
    /// Coerced here rather than in `composite.wgsl` and `commit.wgsl`
    /// separately: those two must be handed the *same* style, and the way that
    /// is guaranteed is that there is only one place it is decided.
    #[test]
    fn a_stroke_carries_its_brushs_blend_mode_except_where_it_means_nothing() {
        let mut ed = Editor::default();
        ed.brush.blend = BlendMode::Multiply;

        assert!(ed.begin_stroke(point()));
        assert_eq!(ed.stroke_style.blend, BlendMode::Multiply);

        ed.interaction = Interaction::Idle;
        ed.brush.mode = BrushMode::Erase;
        assert!(ed.begin_stroke(point()));
        assert_eq!(
            ed.stroke_style.blend,
            BlendMode::Normal,
            "an eraser has no colour to blend"
        );

        ed.interaction = Interaction::Idle;
        ed.brush.mode = BrushMode::Paint;
        ed.layers.add_mask(0);
        ed.edit_target = EditTarget::Mask;
        assert!(ed.begin_stroke(point()));
        assert!(ed.stroke_style.on_mask);
        assert_eq!(
            ed.stroke_style.blend,
            BlendMode::Normal,
            "a mask holds coverage, not colour"
        );

        assert_eq!(
            ed.brush.blend,
            BlendMode::Multiply,
            "the brush itself is untouched — the coercion is the stroke's"
        );
    }

    /// **The flip is wired, and this is what says so.**
    ///
    /// `LayerStack::flip_effects` being correct is `umber-core`'s business and is
    /// tested there; what cannot be tested there is that anything calls it. That
    /// is the gap `LayerStack::flip_text`'s own docs name in as many words —
    /// "nothing calls it yet" — and the one an effect's flip had no note about at
    /// all, which is why a second agent had to find it. A test at the call site is
    /// worth more than a note, so here is one.
    ///
    /// `Editor::flip_canvas` rather than `App::mirror_document` because that is
    /// where a flip reaches the *model*: the selection's mirror is already there,
    /// and `mirror_document` is the one route to it.
    #[test]
    fn flipping_the_canvas_mirrors_an_effects_lighting() {
        let mut ed = Editor::default();
        let shadow = umber_core::Effect {
            angle: 120.0,
            distance: 10.0,
            ..umber_core::Effect::drop_shadow()
        };
        assert!(ed.layers.set_effect(0, shadow));
        let (was_x, _) = shadow.offset();

        ed.flip_canvas(umber_core::FlipAxis::Horizontal);
        let there = ed
            .layers
            .get(0)
            .and_then(|l| l.effect(umber_core::EffectKind::DropShadow))
            .copied()
            .expect("the effect survived");
        let (dx, _) = there.offset();
        assert!(
            (dx + was_x).abs() < 1e-3,
            "a flip left the shadow cast the way it was: {dx} against {was_x}"
        );
    }

    // --- layer effects ------------------------------------------------------

    /// **A document with no effects produces the draw list it always did, entry
    /// for entry.** `docs/layer-effects.md` §11 calls this the regression that
    /// matters most, and it is the one that needs no device.
    ///
    /// Stated over the effect-carrying constructor as well, because
    /// [`Editor::layer_draws`] is now written in terms of it: the two agreeing is
    /// what stops one of them learning about folders, or about a float's preview
    /// slice, and the other not.
    #[test]
    fn a_document_with_no_effects_produces_the_draw_list_it_always_did() {
        let mut ed = with_a_folder();
        ed.layers.get_mut(0).expect("a layer").visible = false;
        ed.layers.get_mut(2).expect("a layer").clipped = true;

        let plain = ed.layer_draws(None);
        let effected = ed.effected_draws(None);
        assert_eq!(plain.len(), effected.len());
        for (a, b) in plain.iter().zip(&effected) {
            assert_eq!(a.slot, b.draw.slot);
            assert_eq!(a.visible, b.draw.visible);
            assert_eq!(a.clipped, b.draw.clipped);
            assert!(
                b.effects.is_empty(),
                "a layer nobody gave an effect carries none"
            );
        }
    }

    /// The float's preview slice is substituted in both readings.
    ///
    /// The one thing `layer_draws` does that a caller could not, and it has to
    /// survive being written in terms of `effected_draws` — a float whose effects
    /// were baked from the layer's own slice would leave a shadow standing where
    /// the picture was, every frame of the drag.
    #[test]
    fn a_floats_preview_slice_reaches_both_readings() {
        let ed = Editor::default();
        let slot = ed.layers.layers()[0].slot().expect("a layer has a slot");
        let float = Some((slot, 9));
        assert_eq!(ed.layer_draws(float)[0].slot, 9);
        assert_eq!(ed.effected_draws(float)[0].draw.slot, 9);
    }

    /// Effect slices start above everything the model has *claimed*, not above
    /// everything the draw list names.
    ///
    /// The difference is a slice parked in an undo entry: claimed, in no layer,
    /// and therefore in no draw. An effect written there would be an effect
    /// written over a deleted layer's pixels, found when somebody undid the
    /// delete — so this reads `slot_capacity_needed`, and the `+ 1` is the slice a
    /// floating transform previews into.
    #[test]
    fn effect_slices_start_above_every_slice_the_model_has_claimed() {
        let mut ed = Editor::default();
        ed.layers.add();
        ed.layers.add();
        let needed = ed.layers.slot_capacity_needed();
        assert_eq!(ed.effect_slot_base(), needed + 1);
        let highest = ed
            .effected_draws(None)
            .iter()
            .map(|e| e.draw.slot)
            .max()
            .expect("some draws");
        assert!(
            ed.effect_slot_base() > highest + 1,
            "an effect slice would collide with the float's spare"
        );
    }

    // --- folders ------------------------------------------------------------

    /// A stack with a folder holding the top two of three layers.
    fn with_a_folder() -> Editor {
        let mut ed = Editor::default();
        ed.layers.add();
        ed.layers.add();
        ed.layers.group(&[1, 2]).expect("the top two");
        ed
    }

    /// **A folder never reaches the composite.** This is the whole of why
    /// `composite.wgsl` was not touched for folders, and why the four other
    /// things that reuse that pass — `export_rgba`, `pick_colour`,
    /// `probe_canvas` and the autosave's capture — needed nothing at all.
    #[test]
    fn a_folder_is_flattened_out_of_the_draw_list() {
        let ed = with_a_folder();
        assert_eq!(ed.layers.len(), 4, "three layers and one folder");
        let draws = ed.layer_draws(None);
        assert_eq!(draws.len(), 3, "the folder contributes no draw");
        let slots: Vec<u32> = draws.iter().map(|d| d.slot).collect();
        let expected: Vec<u32> = ed.layers.layers().iter().filter_map(|l| l.slot()).collect();
        assert_eq!(slots, expected, "and the rest are in stack order");
    }

    /// A folder's eye folds into its contents, because visibility is a boolean
    /// and `hidden ∧ anything = hidden`. An *opacity* would not fold, which is
    /// why a pass-through folder has none — see `docs/layer-folders.md`.
    #[test]
    fn hiding_a_folder_hides_its_contents_in_the_draw_list() {
        let mut ed = with_a_folder();
        assert!(ed.layer_draws(None).iter().all(|d| d.visible));

        ed.layers.get_mut(3).unwrap().visible = false;
        let draws = ed.layer_draws(None);
        assert_eq!(
            draws.iter().map(|d| d.visible).collect::<Vec<_>>(),
            vec![true, false, false],
            "the layer outside the folder still draws"
        );
        assert!(
            ed.layers.get(1).unwrap().visible,
            "the layers' own eyes are untouched, so opening the folder reveals \
             them again"
        );
    }

    /// **A draw's position is not a stack position** once there is a folder in
    /// the document, and the composite is told which draw carries the stroke in
    /// flight. Getting this wrong previews the stroke on the wrong layer —
    /// under the wrong blend mode, at the wrong opacity — and it then jumps at
    /// pointer-up when the commit puts it where it really went.
    #[test]
    fn the_stroke_is_previewed_on_the_draw_the_layer_actually_is() {
        let mut ed = Editor::default();
        ed.layers.add();
        ed.layers.add();
        // Group the *bottom* layer, so the folder sits below the other two and
        // shifts every draw index above it.
        ed.layers.group(&[0]).expect("one layer in a group");
        // Stack: [Layer 1 (in), Group 1, Layer 2, Layer 3]
        assert_eq!(ed.layers.len(), 4);

        for (stack, draw) in [(0usize, 0u32), (2, 1), (3, 2)] {
            ed.layers.set_active(stack);
            assert_eq!(
                ed.active_draw_index(),
                draw,
                "stack position {stack} is draw {draw}"
            );
            assert_eq!(
                ed.layer_draws(None)[draw as usize].slot,
                ed.layers.active_slot().unwrap()
            );
        }
    }

    /// A folder selected answers with a position past the end, which the
    /// shader's `i == active_index` simply never matches. Deliberately not "the
    /// layer below it", which is a real draw and would preview the stroke on a
    /// layer nobody chose.
    #[test]
    fn a_selected_folder_carries_no_stroke() {
        let mut ed = with_a_folder();
        ed.layers.set_active(3);
        assert!(ed.layers.active_is_folder());
        assert_eq!(ed.active_draw_index(), u32::MAX);
        assert!(
            ed.active_draw_index() as usize >= ed.layer_draws(None).len(),
            "no draw may match a folder"
        );
    }

    /// The gate. A folder holds neither pixels nor a mask, so there is nowhere
    /// for a stroke to land — refused at the same one place a lock is, rather
    /// than at the four routes that reach it.
    #[test]
    fn a_folder_refuses_a_stroke() {
        let mut ed = with_a_folder();
        ed.layers.set_active(3);
        assert_eq!(ed.stroke_target(), None);
        assert!(!ed.begin_stroke(point()), "a folder must refuse");
        assert_eq!(
            ed.interaction,
            Interaction::Idle,
            "a refused stroke must not leave the pointer drawing"
        );
        assert!(!ed.stroke.is_active());
    }

    /// A lock on a folder reaches every layer inside it, through the same one
    /// gate every operation asks.
    #[test]
    fn a_lock_on_a_folder_refuses_a_stroke_on_what_is_inside_it() {
        let mut ed = with_a_folder();
        ed.layers.set_active(1);
        assert!(ed.begin_stroke(point()), "unlocked, so it paints");
        ed.stroke.end();
        ed.interaction = Interaction::Idle;

        ed.layers.get_mut(3).unwrap().locked = true;
        assert!(
            !ed.begin_stroke(point()),
            "the folder's lock has to reach it"
        );
        assert_eq!(ed.interaction, Interaction::Idle);
    }
}
