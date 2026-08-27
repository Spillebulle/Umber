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

/// How far the pointer has to travel before the lasso records another sample,
/// in *screen* pixels. Divided by the zoom at the point of use.
///
/// One pixel, which is the finest the screen can distinguish and therefore the
/// finest the hand can ask for. It used to be one *document* pixel, which was a
/// bound that bit hardest where there was least to bound — see
/// `SelectionDraft::moved`.
const SELECT_SAMPLE_PIXELS: f32 = 1.0;

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
    /// A colour is being taken, and the pointer is still down.
    ///
    /// It exists so that the eyedropper is a *drag* rather than a click, which
    /// is what lets the sample follow the pointer out of the window and onto
    /// the desktop — winit keeps delivering moves while a button is held, see
    /// [`crate::syspick`]. One interaction rather than a flag on the tool,
    /// because the same drag has to work for the tool in hand and for Alt held
    /// with any other tool: both resolve to `gesture::Press::Eyedropper`, and
    /// this is where that one answer lands.
    Picking,
}

impl Interaction {
    /// May the eyedropper be *aimed* while this is going on?
    ///
    /// Idle is the hover, Picking is the drag itself, and everything else is
    /// another gesture holding the pointer — so the crosshair comes off and
    /// nothing is read. That last part is what makes this more than tidiness:
    /// a middle-drag pans with any tool selected, so without it a pan with the
    /// eyedropper in hand drew a loupe over a canvas sliding underneath it,
    /// promising a colour the release would not take, and paid a blocking GPU
    /// readback on every frame of a gesture that had cost nothing.
    ///
    /// An exhaustive `match` and not a `matches!`, which is CLAUDE.md's
    /// standing rule: this answers `false` by *decision* for four
    /// interactions, and a `matches!` would answer it by accident for a fifth
    /// nobody had thought about.
    pub fn allows_aim(self) -> bool {
        match self {
            Self::Idle | Self::Picking => true,
            Self::Drawing | Self::Selecting | Self::Panning | Self::Zooming => false,
        }
    }
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
/// transform moves what they marked, the eyedropper takes a colour, and pan and
/// zoom navigate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Brush,
    Eraser,
    Select,
    Transform,
    /// Take the colour under the pointer. Alt with any other tool in hand does
    /// the same thing for one press; this is the tool for when that is the
    /// whole of what somebody is doing, and it is the only way to reach a
    /// colour that is *outside* the window — see [`crate::syspick`].
    Eyedropper,
    Pan,
    Zoom,
}

impl Tool {
    /// Whether a press with this tool lays paint down.
    ///
    /// **A `match` and not a `matches!`**, which is this codebase's standing
    /// rule and was earned rather than guessed: `matches!` answers *false* for
    /// a variant it has never heard of, so a tool added later silently becomes
    /// one that does not paint — and the options strip, which asks this to
    /// decide whether to draw the size and opacity rails, would go quiet about
    /// it with nothing failing to build. See CLAUDE.md, "Partial
    /// exhaustiveness is worse than none".
    pub fn paints(self) -> bool {
        match self {
            Self::Brush | Self::Eraser => true,
            Self::Select | Self::Transform | Self::Eyedropper | Self::Pan | Self::Zoom => false,
        }
    }
}

/// Pixels picked up off a layer — or pasted onto one — being moved about.
///
/// Transient like [`Editor::stroke`], and for the same reason it sits above the
/// `--- documents ---` line: every path that would leave the document behind
/// puts it down first, so it never has to travel. Its pixels live in the
/// renderer; what is here is only where they have been dragged to.
#[derive(Clone, Debug)]
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
    /// The layer this float **made for itself**, where it made one.
    ///
    /// Only a text placement does, and only where `Editor::ui.text_own_layer` is
    /// on. It rides on the float rather than beside [`Editor::float_text`] on
    /// purpose, and the reason is that field's own docs read backwards:
    /// `float_text` is cleared by `App::begin_float` and deliberately *not* by
    /// `App::cancel_transform`, so a duty hung on it would be a duty the cancel
    /// path forgets — and what would be forgotten here is an empty layer left in
    /// somebody's stack every time they press Escape. Every path that abandons a
    /// float takes `float` itself, so this is the one place that cannot be
    /// missed.
    ///
    /// [`MadeLayer`] carries the stack shape the placement's undo entry is made
    /// of, which owns a `Vec`, so this is `Clone` and no longer `Copy`. It was
    /// plain numbers while the entry was recorded at the *add* and the shape
    /// therefore lived in the history; [`MadeLayer::before`] says why it is not
    /// recorded there any more. Nothing was lost by the change: every path that
    /// ends a gesture already takes the float out of the `Option`, and the two
    /// that merely read one now borrow it.
    pub made: Option<MadeLayer>,
}

/// A layer a float created for itself, what was selected before it did, and the
/// stack shape its undo entry will be made of.
///
/// The first two are [`umber_core::Layer::id`]s, never indices and never slots:
/// this is written at the placement and read a whole gesture later, and an index
/// stops meaning this layer the moment anything is reordered.
#[derive(Clone, Debug)]
pub struct MadeLayer {
    /// The entry the placement added.
    pub id: u32,
    /// The entry that was selected before it, so an undo puts the selection back
    /// where the artist left it rather than on whatever now sits in that row.
    pub was_active: u32,
    /// The stack as it stood **before** the layer was added, waiting to become
    /// the placement's one undo entry when the commit records it.
    ///
    /// **Nothing is recorded until the commit, and that is what makes Escape
    /// free.** The layer is empty until the commit writes into it, so abandoning
    /// a placement really does leave the document exactly as it was found — and
    /// `History::record` drains the redo stack, which [`Editor::unmake_layer`]
    /// could never put back. Undo a stroke, place a caption, press Escape, and
    /// with the entry recorded at the add the stroke could no longer be redone.
    ///
    /// **It could not be recorded here while a reorder could land in the middle
    /// of the gesture**, and that is the whole history of this field. The layer
    /// stands in the stack for the whole placement; an entry recorded at the end
    /// of it sits on the undo stack *above* anything recorded in between, so a
    /// `MoveLayer` made in that window is undone second — by which time the
    /// placement's own undo has taken the layer out, and the reorder's shape
    /// names an entry that is gone. `LayerStack::restore_shape` refuses such a
    /// shape whole and hands it straight back, which `App::reverse` reads as
    /// `Ok`: the history moves and the picture does not.
    ///
    /// So the reorder was stopped instead. Every other structural edit already
    /// settled the float before recording; the two that did not were the Layers
    /// panel's chevrons and its drag, and both do now — see `App::record_move`,
    /// which the drag reaches through `UiActions::reorder_layer` because
    /// settling needs the renderer and a panel has none. With no structural edit
    /// able to interleave, the placement's entry is the newest one when it is
    /// finally recorded and every shape is placeable when it is reached.
    ///
    /// Snapshotted rather than rebuilt at the commit. Rebuilding it would mean
    /// "the stack as it stands, minus this layer", which is only the same thing
    /// for as long as nothing else moved the stack — a discipline claim, where a
    /// snapshot is a fact.
    pub before: Box<umber_core::StackShape>,
}

/// "one text layer" or "3 text layers", for a notice that has to count them.
///
/// A function rather than `"{n} text layer(s)"` because a bracketed plural is a
/// thing a program writes and not a thing a person does, and this project's own
/// rule for user-facing text is that it reads the way somebody writes. One is
/// spelled out for the same reason: "1 text layer" is a form filling in a field.
fn text_layers(count: usize) -> String {
    if count == 1 {
        "one text layer".to_string()
    } else {
        format!("{count} text layers")
    }
}

/// What a floating block of text was set from, waiting for the commit that will
/// record it on the layer.
///
/// Only what [`umber_core::textobj::TextObject`] needs and does not already
/// have: the placement comes from the [`Transform`] at the moment it is put
/// down, because that is the whole of what the artist did to it in between.
#[derive(Clone, Debug)]
pub struct PlacedText {
    pub block: umber_core::text::TextBlock,
    pub face: umber_core::textobj::TextFace,
    /// The colour the coverage was painted in, snapshotted at the placement for
    /// the reason [`Editor::stroke_style`] is snapshotted at the press: the
    /// palette can move while the box is being dragged, and the record has to
    /// describe the pixels that actually land.
    pub colour: Color,
    /// How large the setting was, in pixels, before `Clip::place` saw it.
    ///
    /// **Carried so the commit can tell a placement that was cropped.** A block
    /// larger than the canvas is centred and cropped — `float_a_clip` says so in
    /// a notice — and the source rectangle then records the *cropped* size while
    /// the next Update measures the whole block again at identity. The caption
    /// would jump the first time somebody fixed a typo in it. A cropped
    /// placement therefore records nothing at all, which is the same answer one
    /// landing on paint gets and for the same reason: a record that does not
    /// describe the pixels is worse than no record.
    pub size: glam::UVec2,
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
    /// than four tools: see `umber_core::selection`.
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
    /// How far a rectangular selection's corners are rounded, `0.0` square
    /// through `1.0` stadium.
    ///
    /// Interface state on the same footing as the feather, and drawn only in
    /// the mode that answers to it — `SelectionMode::extra`. It is kept while
    /// another mode is in hand rather than reset, for the reason the feather
    /// is: a setting the artist chose is theirs until they change it, and a
    /// rail that emptied itself when they went to the lasso and back would be
    /// one they had to set twice.
    pub selection_roundness: f32,
    /// How heavily the lasso damps the hand, `0.0` off through
    /// `SelectionDraft::MAX_STABILISER`.
    ///
    /// Zero by default, unlike `Brush::stabilization`. A brush stroke is a mark
    /// somebody wants smooth; a selection outline is a boundary they are aiming
    /// at, and one that trails the pointer is one that lands somewhere they did
    /// not point. So the default is the behaviour that was there before this
    /// control existed, exactly.
    pub selection_stabiliser: f32,
    pub picker: PickerMode,
    pub wheel_shape: WheelShape,
    /// Whether the wheel's triangle turns to follow the hue. Meaningless for the
    /// square, which has no corner that is the hue to keep beside the marker.
    pub wheel_rotates: bool,
    /// Whether the wheel's triangle has its white and black corners the other
    /// way round.
    ///
    /// A preference rather than something the session forgets, and that is the
    /// point of it: which corner is light is an arrangement somebody arrives
    /// with from another application, sets once, and never thinks about again.
    /// Meaningless for the square, which has no corner standing for either —
    /// see `WheelShape::can_swap_ends`.
    ///
    /// **One flag rather than one per shape, unlike `wheel_angles`**, and the
    /// two differ for a reason: an angle means something for *both* centres and
    /// stands for a different pose in each, where this means something for one
    /// of them and nothing at all for the other. It is `wheel_rotates`' shape,
    /// which is the setting it sits beside on the panel. A third centre with
    /// corners would want its own, and would want a key per shape in the
    /// preferences file the way `wheel_angle_key` already writes one — that is
    /// a migration and it is cheaper than carrying a table for one live value.
    pub wheel_mirrored: bool,
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
    /// Whether placing text makes a layer of its own to hold it.
    ///
    /// **On by default, and the default is the whole feature.** A placement only
    /// keeps its [`umber_core::TextObject`] where it lands on nothing — see
    /// `App::finish_transform` — so text put down over a picture is paint from
    /// the moment it touches the canvas and can never be set again. Its own
    /// layer makes "lands on nothing" true by construction, which is what turns
    /// "text you can edit again" from something that depends on where you
    /// clicked into something that simply holds.
    ///
    /// It is a preference and not a fixed policy because placing a caption
    /// straight onto the layer under it is a real way to work — somebody
    /// flattening as they go — and because at `LayerStack::MAX` there is no new
    /// layer to be had and the artist needs a way through. Its control is on the
    /// Text panel beside Place rather than in the settings dialog, which is
    /// `Prefs::wheel_rotates`' arrangement and its argument: where a setting is
    /// changed does not decide whether it should still be true tomorrow, and the
    /// only place this one means anything is the button it governs.
    pub text_own_layer: bool,
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

/// What one step of the history needs of the document before it can be carried
/// out, or why it cannot be.
///
/// The answer to [`Editor::undo_gate`] and [`Editor::redo_gate`], and the reason
/// this is an enum rather than a `bool` is that the two non-`Clear` answers ask
/// for opposite things: one says *settle the document and go on*, the other says
/// *leave the entry where it is*. A caller that collapsed them would either
/// refuse every flip or quiet the document for a refusal it is about to make.
///
/// **Every reader today matches exhaustively, and a test is what keeps it that
/// way.** The two menu rows read `gate == StepGate::FlipLocked`, which is
/// `matches!` wearing an operator: a fourth answer would have been a compile
/// error in `App::settle_step` and a silent `false` in both rows — the menu
/// going on offering a command the model had just learned to decline, in a
/// change whose whole purpose is stopping exactly that. They go through
/// [`StepGate::refuses`] now. See CLAUDE.md's "Partial exhaustiveness is worse
/// than none"; this is the shape it describes, found by a critic in the diff
/// that cited that section.
///
/// The first phrasing of this paragraph claimed exhaustiveness outright, which
/// is a claim about the whole program that nothing enforces — `PartialEq` is
/// still derived (three tests want `assert_eq!`), so a fifth reader can write
/// the equality test again, which is precisely how this arrived. What holds the
/// line is `ui::tests::the_edit_menus_history_rows_go_dead_when_a_lock_refuses_
/// the_flip`, a guard at the call site. Structure narrows the mistake; only a
/// test at the call site catches it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepGate {
    /// Nothing in the way. Every entry that swaps pixels or stack shape, and
    /// the empty stack.
    Clear,
    /// A canvas flip. The mirror is a GPU permutation of every layer slice, so
    /// the live stroke has to be committed and any autosave capture cancelled
    /// first — `flip_canvas` does both before it mirrors and stepping over one
    /// has to do the same.
    SettleForFlip,
    /// A canvas flip a locked layer refuses. The entry stays where it is, and
    /// the artist is told: a flip that half happened cannot be undone by
    /// flipping again.
    FlipLocked,
}

impl StepGate {
    /// Does this answer refuse the step outright?
    ///
    /// **Exhaustive, and that is the whole reason it exists** rather than the
    /// two call sites comparing against [`StepGate::FlipLocked`]. A control asks
    /// "may I offer this", which is a question about the *set* of refusing
    /// answers, and an equality test answers it only for as long as that set has
    /// one member. See the type's own docs.
    pub fn refuses(self) -> bool {
        match self {
            Self::Clear | Self::SettleForFlip => false,
            Self::FlipLocked => true,
        }
    }
}

/// Why a locked layer refuses a canvas flip, in one clause.
///
/// **Three controls have to say this**: the Image menu's two flip rows, the Edit
/// menu's Undo and Redo rows, and the notice a refused keystroke raises. They
/// were three hand-written near-copies, which is the drift
/// [`Editor::flip_refused_by_lock`] was introduced two commits earlier to stop
/// for the *reading* — the sentence deserves the same treatment and a critic
/// pointed out that it had not had it.
///
/// Written so it is true whether one layer is locked or twenty: `any_locked` is
/// an `iter().any()`, so the "Unlock **it** first" it replaces is wrong for a
/// stack with three locked in it, and a plural-agreement dance is not worth the
/// alternative. Only that half needed fixing — "cannot skip one" has *every
/// layer* for its antecedent and was always right, and saying "a locked one"
/// instead put a third "lock" into one tooltip for nothing. No em-dash, like
/// everything else the interface draws.
pub const FLIP_LOCKED_REASON: &str = "A flip mirrors every layer at once, so it cannot skip one. Unlock every \
     locked layer in the Layers panel";

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
            selection_roundness: 0.0,
            selection_stabiliser: 0.0,
            picker: PickerMode::Wheel,
            wheel_shape: WheelShape::Triangle,
            // What the picker has always done, and what the design draws.
            wheel_rotates: true,
            // Off is the arrangement every build before the swap existed drew,
            // which is also what a preferences file written by one supplies.
            wheel_mirrored: false,
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
            text_own_layer: true,
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
    /// A document being decoded on a worker, if one is.
    ///
    /// Above the `--- documents ---` line deliberately: it is not a property of
    /// any document — it is the one that has not become a document yet — and a
    /// tab switch must not carry it about or abandon it. See `loading.rs`.
    pub loading: Option<crate::loading::Loading>,
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
    /// What the floating pixels were **set from**, where they were set rather
    /// than pasted or lifted.
    ///
    /// Beside [`Editor::float`] rather than a field on `Floating`, which is
    /// `Copy` and read by value at three call sites; a `TextBlock` holds a
    /// `String`, so folding it in would take the `Copy` away from all of them
    /// for a field only the commit reads.
    ///
    /// **`App::begin_float` clears it unconditionally as it installs a float,
    /// and that one line is the whole guarantee.** `App::place_text` sets it
    /// immediately afterwards and `App::finish_transform` takes it, so a
    /// placement the artist abandoned cannot attach itself to the next paste's
    /// commit.
    ///
    /// The three places that clear [`Editor::float`] and leave this alone —
    /// `App::cancel_transform`, `Editor::install_document` and `App::suspended`
    /// — are therefore safe rather than forgotten, and they are named here
    /// because "there is no third site" was written first and was not true.
    /// Nothing reads this without a float, and no float exists that
    /// `begin_float` did not install.
    ///
    /// Above the `--- documents ---` line with the float itself, and for the
    /// same reason: every path that leaves the document commits first.
    pub float_text: Option<Box<PlacedText>>,
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
    /// What the eyedropper's magnifier is showing, or `None` for no pick aimed.
    ///
    /// Above the `--- documents ---` line with `input` and for the same reason:
    /// it describes where a pointer is and what is under it, which belongs to
    /// the gesture rather than to any picture — and a tab switch abandons the
    /// gesture, exactly as it abandons a `SelectionDraft`.
    ///
    /// Written once per frame by `App::pick_this_frame`, which is the one place
    /// a pixel is read; read only by `ui::loupe`, which paints it. Where it
    /// goes and what it may hold are `crate::loupe`'s, in a model with no
    /// drawing in it.
    pub loupe: Option<crate::loupe::Loupe>,
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
            loading: None,
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
            float_text: None,
            clipboard: None,
            selection_draft: None,
            selection_outline: Vec::new(),
            selection_screen: Vec::new(),
            selection_dashes: Vec::new(),
            history: History::default(),
            pressure: PressureModel::default(),
            input: crate::inputlog::InputLog::default(),
            loupe: None,
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
            Tool::Select | Tool::Transform | Tool::Eyedropper | Tool::Pan | Tool::Zoom => {}
        }
    }

    /// Adopt the picker's HSV as the painting colour.
    pub fn commit_picker(&mut self) {
        self.color = self.hsv.to_color(1.0);
    }

    /// What the Text module will set in.
    ///
    /// The palette colour, unless the panel is **editing a layer's record**, in
    /// which case it is the colour that record holds. A text layer set again in
    /// whatever happened to be in the palette would change its colour every time
    /// somebody fixed a typo, which is the picture changing behind an edit that
    /// did not ask for it; `textpanel`'s "Use the colour in hand" is how the
    /// artist asks.
    ///
    /// One reading, here, because three things have to agree about it — the
    /// preview, the key the preview is cached under, and what Place and Update
    /// actually paint.
    pub fn text_colour(&self) -> Color {
        self.text.editing.as_ref().map_or(self.color, |e| e.colour)
    }

    /// Make the layer a text placement puts its words on, and carry the shape
    /// its one undo entry will be made of.
    ///
    /// The model half of `App::add_text_layer` — the stack, the name and the
    /// snapshot, and nothing that needs a device. It is here rather than there
    /// **so that it can be driven with no window**: a critic deleted the
    /// recording when it lived at the call site and all 856 tests stayed green,
    /// which is the "a guard on a model is not a guard on the call site" failure
    /// with a crate boundary in the way. Split like this, the model is what the
    /// guards can reach — which is why [`Editor::commit_made_layer`] is here
    /// beside it rather than being three lines inside `App::finish_transform`.
    ///
    /// **Nothing is recorded here**, and that is what makes Escape free. See
    /// [`MadeLayer::before`].
    ///
    /// `None` where the stack would not take another layer. The caller has
    /// already asked `LayerStack::add_refusal` in order to say *why*, so this
    /// answering at all is the belt to that gate's braces.
    pub fn make_text_layer(&mut self, text: &str) -> Option<MadeLayer> {
        let name = umber_core::textobj::layer_name(text);
        let was_active = self
            .layers
            .get(self.layers.active_index())
            .map_or(0, |l| l.id());
        // Before the add, so what it describes is the stack without this layer
        // in it — restoring it is what takes the layer back out. Taken before
        // the `?`, which costs a `Vec` of ids on a refusal and is the order that
        // cannot snapshot a stack the add has already changed.
        let before = self.layers.shape(self.doc.layer_bytes());
        let made = self.layers.add_named(&name)?;
        Some(MadeLayer {
            id: made.id,
            was_active,
            before: Box::new(before),
        })
    }

    /// Record the one undo entry a placement onto its own layer gets.
    ///
    /// Called from `App::finish_transform` once the commit is certain to
    /// happen, and from nowhere else. The kind is `EditKind::AddLayer` rather
    /// than a variant of its own because that row undoes exactly as an add does,
    /// and two rows that undo identically must not have two names — the rule
    /// that already files a paste under Transform and a cut under Erase.
    ///
    /// It needs no patch and no readback: taking the layer back out takes its
    /// pixels with it, because a `ShapeEntry::Gone` carries the whole `Layer`
    /// and its `SlotClaim`. So one Ctrl+Z takes back the layer, the words on it
    /// and its record together.
    ///
    /// On `Editor` rather than written out at the call site for the reason
    /// [`Editor::make_text_layer`] is: a recording inside `App` is a recording
    /// no guard in this crate can drive without a device, and deleting it left
    /// every test green once already.
    pub fn commit_made_layer(&mut self, made: MadeLayer) {
        self.history.record(umber_core::Edit::new(
            umber_core::EditKind::AddLayer,
            *made.before,
        ));
        self.mark_modified();
    }

    /// Take back a layer a text placement made.
    ///
    /// The counterpart to [`Editor::make_text_layer`], for every way a placement
    /// can end without committing: Escape, a float that was refused, and a block
    /// dragged entirely off the canvas. The layer is empty on all three, because
    /// nothing was written to it until the commit.
    ///
    /// **It touches the history not at all, and that is the whole of why Escape
    /// is free.** [`Editor::commit_made_layer`] is what records, and it runs
    /// only where a commit is certain — so on every path through here there is
    /// no entry to take off, no redo stack drained and nothing for a later undo
    /// to walk past. This used to pop the entry the add had recorded, which
    /// worked for the undo stack and could do nothing at all about the redo one:
    /// `History::record` drains it and `take_undo` cannot put it back, so
    /// pressing Escape lost whatever the artist had undone before they started
    /// typing. See [`MadeLayer::before`].
    ///
    /// **The layer is dropped rather than parked.** A parked slice is held alive
    /// by an undo entry that could put the layer back, and there is no such
    /// entry here at all, so parking would leak a canvas-sized slice per
    /// abandoned placement — 400 MB apiece on a 10000² document, for pressing
    /// Escape.
    ///
    /// By **id**, never by index: this runs a whole gesture after the add.
    pub fn unmake_layer(&mut self, made: Option<MadeLayer>) {
        let Some(made) = made else {
            return;
        };
        let Some(at) = self.layers.layers().iter().position(|l| l.id() == made.id) else {
            return;
        };
        if self.layers.remove(at).is_none() {
            // `remove` refuses to leave a document with nowhere to paint. A
            // placement adds its layer *beside* one that already exists, so this
            // cannot fire; logged rather than asserted because the outcome if it
            // ever did is one extra empty layer, which is a great deal better
            // than a panic on a path the artist reached by pressing Escape.
            log::warn!("a placement's own layer could not be taken back");
            return;
        }
        if let Some(back) = self
            .layers
            .layers()
            .iter()
            .position(|l| l.id() == made.was_active)
        {
            self.layers.set_active(back);
        }
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
        //
        // **[`Editor::unmake_layer`] is deliberately not called**, unlike at the
        // other two sites that take a float. By this line the stacks have
        // already been swapped, so the layer a float made belongs to a document
        // that is no longer here and the id would resolve — if it resolved at
        // all — against the incoming one. Doing nothing leaves an empty layer in
        // the outgoing document with the entry that removes it; reaching for
        // that layer here would remove somebody else's.
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
            // Every text record too, and for the reason the history goes: a
            // placement is a rectangle of a canvas that no longer exists, and a
            // canvas that shrank has cropped the pixels the record describes,
            // so a re-render would put back part of the text the artist cut
            // away. Translating by the anchor's offset would be exact for a
            // canvas that only *grew*, and two behaviours behind one command is
            // how the cropping case comes to be the one nobody tested.
            let dropped = self.layers.drop_text_objects();
            if dropped > 0 {
                // Named rather than discovered. Unlike the flip's, this one is
                // reachable by anybody who resizes a document holding text.
                self.notice = Some(Notice {
                    title: "Text on this document is paint now".to_string(),
                    lines: vec![format!(
                        "The canvas changed size, so Umber no longer knows where the text \
                         on {} goes. Every pixel is still there. What is lost is that it \
                         can be set again.",
                        text_layers(dropped)
                    )],
                });
            }
            // Keep the zoom, but not the ability to be looking at a part of the
            // canvas that no longer exists.
            self.camera.center = self.camera.center.clamp(Vec2::ZERO, doc.size_vec2());
        }
        resized
    }

    /// **The one reading of "may this document be mirrored".**
    ///
    /// A canvas flip is refused *whole* when any layer is locked — see
    /// `App::mirror_document` for why half a flip is not a state the history's
    /// pixel-less entry can describe — and three separate controls have to
    /// agree about it: the Image menu's two flip rows, the Edit menu's Undo and
    /// Redo rows, and the keystroke behind each. One statement here rather than
    /// three `layers.any_locked()` calls that happen to be spelled the same, so
    /// a change to what refuses a flip cannot leave one control offering it.
    pub fn flip_refused_by_lock(&self) -> bool {
        self.layers.any_locked()
    }

    /// What carrying the entry at the top of the undo stack backwards needs of
    /// this document first, or why it cannot be carried out at all.
    ///
    /// The **plan half** of `App::reverse`, in `LayerStack::plan_reorder`'s
    /// shape: the answer is available without spending the entry, so a control
    /// can be disabled to match and the entry can be left alone where it cannot
    /// be carried out. See [`StepGate`] for what the three answers cost.
    pub fn undo_gate(&self) -> StepGate {
        self.gate_for(self.history.next_undo().map(|edit| edit.kind))
    }

    /// [`Editor::undo_gate`]'s twin for the redo stack.
    ///
    /// A flip is refused in both directions, so this is not a courtesy: a redo
    /// that spent an entry it could not carry out would damage the document in
    /// exactly the way an undo does.
    pub fn redo_gate(&self) -> StepGate {
        self.gate_for(self.history.next_redo().map(|edit| edit.kind))
    }

    /// The rule both gates share, over the kind of whichever entry is next.
    ///
    /// `None` — nothing on that stack — answers [`StepGate::Clear`] rather than
    /// a fourth variant: the caller's `take_undo` already returns `None` a line
    /// later, and "there is nothing to step over" is not a refusal anybody has
    /// to be told about. `History::can_undo` is what a control asks about that.
    ///
    /// **This predicts on the `kind` where `App::reverse` decides on the
    /// `body`, and the asymmetry is deliberate.** `reverse` mirrors only for an
    /// `EditBody::Flip` whose kind also names an axis; this refuses for the kind
    /// alone. The kind's predicate is therefore a strict *superset* of the
    /// body's, so this can never answer `Clear` over a step that will go on to
    /// mirror — it fails closed, which is the direction that matters. Reading
    /// the body would be exact and is one line away, since `next_undo` hands
    /// back the whole `&Edit`; it is not taken, because the exact reading buys
    /// nothing over a safe over-approximation and would make the two functions
    /// agree by construction *only* while both are read the same way. The guard
    /// builds the disagreeing state on purpose — a `Flip` body under every kind
    /// — which pins this reading rather than the other.
    fn gate_for(&self, next: Option<umber_core::EditKind>) -> StepGate {
        // Read off `flip_axis` rather than `matches!` on the two flip variants,
        // so a third axis would arrive here already handled. The axis itself is
        // `reverse`'s to read back off the kind — carrying it in the gate would
        // be a second copy of a number that has exactly one source.
        match next.and_then(umber_core::EditKind::flip_axis) {
            None => StepGate::Clear,
            Some(_) if self.flip_refused_by_lock() => StepGate::FlipLocked,
            Some(_) => StepGate::SettleForFlip,
        }
    }

    /// Mirror the live document, in everything but its pixels.
    ///
    /// The pixels are the renderer's — see `CanvasRenderer::flip_layers` — and
    /// the history entry is `app.rs`'s, because it has to be recorded only if
    /// the GPU work actually happened. What is here is everything on the document
    /// that carries a **direction or a position**: the selection, which is
    /// geometry, every layer effect's lighting, and every text record's
    /// placement.
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
        // shadows disagreeing with its forms.
        self.layers.flip_effects(axis);
        // And a text record carries a *position*, which is the same job: a
        // placement left alone would put the next re-render where the text used
        // to be, un-mirroring the layer. The mirror is exact — see
        // `Placement::flipped` — which is why a flip does not cost a text layer
        // its record the way a resize does, and it has to be, because undoing a
        // flip is another flip.
        let dropped = self.layers.flip_text(axis, doc);
        if dropped > 0 {
            // Rare rather than unreachable, and the difference is worth stating.
            // `Clip::place` crops to the document, so a *placement* never
            // produces one; what can is `App::update_text_layer`, which grows
            // the source rectangle with the ink, so a caption set much longer
            // near the right edge can end up with a source that runs off the
            // canvas even though what was drawn was clamped to it. Said out loud
            // rather than logged, because a record that lies about where its
            // pixels are is worse than none and somebody whose caption stopped
            // being editable is owed the reason.
            self.notice = Some(Notice {
                title: "Some text is paint now".to_string(),
                lines: vec![format!(
                    "{} sat outside the canvas, so Umber could not mirror where the text \
                     goes. Every pixel is still there. What is lost is that it can be set \
                     again.",
                    text_layers(dropped)
                )],
            });
        }
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
    /// **Exhaustive over [`EditTarget`] on purpose, and it was not.** It read
    /// `match (self.edit_target, self.layers.active_mask())` with a single
    /// `_ =>` falling through to the layer's slot, which is `matches!` wearing
    /// a tuple: a third variant added to `EditTarget` would be a stroke landing
    /// silently on the layer's *pixels*, which is the one outcome that damages
    /// a document rather than refusing to touch it. Found by an agent whose own
    /// gate had cited this function as a precedent for exhaustive matching.
    /// See CLAUDE.md's "Partial exhaustiveness is worse than none" — the
    /// catch-all is what makes the compiler look as though it has your back.
    ///
    /// The fall-through for `Mask` **with no mask** is deliberate and stays,
    /// which is why the arms are written out rather than collapsed: it is a
    /// stated answer to a real state, not a default for one nobody considered.
    pub fn stroke_target(&self) -> Option<(u32, bool)> {
        match self.edit_target {
            EditTarget::Mask => match self.layers.active_mask() {
                Some(slot) => Some((slot, true)),
                None => Some((self.layers.active_slot()?, false)),
            },
            EditTarget::Layer => Some((self.layers.active_slot()?, false)),
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

    /// Begin a stroke, unless the layer it would land on refuses one.
    ///
    /// **Every refusal is made here and nowhere else on the painting path.**
    /// Every route to a stroke — mouse, pen, touch, a shortcut that starts one —
    /// arrives at this function, so a check spread over those call sites is one
    /// that a later route would miss. Returns whether the stroke started, which
    /// is what tells the caller not to put the interaction into `Drawing`.
    ///
    /// **The reading is `LayerStack::refusal_at`'s**, which is one call with one
    /// answer for the lock, the folder and the text record together. The lock
    /// and the folder used to be two separate tests here, which is exactly the
    /// arrangement that gate exists to replace: text was the third and a third
    /// test is the one somebody forgets.
    ///
    /// **The target has to be the one `stroke_target` actually resolved to, not
    /// the one the strip says**, and that is the half that is easy to get
    /// backwards. `EditTarget::Mask` on a layer with no mask falls back to the
    /// *layer*, so asking about the mask there would let a brush onto a text
    /// layer's own pixels and leave its record describing pixels it did not
    /// make. Everything is silent, because this is reached every time the pen
    /// goes down and a notice there would be a dialog over the canvas.
    pub fn begin_stroke(&mut self, point: InputPoint) -> bool {
        // A folder holds no pixels and no mask, so there is nowhere for the
        // stroke to go. `refusal_at` answers `Folder` for one too; this is what
        // produces the slice, and it is what decides which target to ask about.
        let Some((slot, on_mask)) = self.stroke_target() else {
            return false;
        };
        let target = if on_mask {
            EditTarget::Mask
        } else {
            EditTarget::Layer
        };
        if self.layers.active_refusal(target).is_some() {
            return false;
        }
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
    /// Only the polygon can see a second press: the other three modes are one
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
                // Every setting the strip carries is snapshotted here, in one
                // place, for the reason the operation is: a polygon spans
                // several clicks and a rail dragged between two of them must
                // not change what the gesture already under way turns out to
                // have meant. `SelectionMode::extra` decides which of the last
                // two the strip actually drew, and the draft records both
                // regardless — the mode is what ignores the one it has no use
                // for, so nothing here has to be kept in step with the strip.
                self.selection_draft = Some(
                    SelectionDraft::new(self.ui.selection_mode, doc)
                        .combining(op)
                        .feathered(self.ui.selection_feather)
                        .rounded(self.ui.selection_roundness)
                        .stabilised(self.ui.selection_stabiliser),
                );
                self.interaction = Interaction::Selecting;
            }
        }
    }

    /// How far the pointer must travel before the lasso records another sample,
    /// in document pixels.
    ///
    /// A screen distance divided by the zoom, exactly as
    /// [`Editor::handle_tolerance`] and `selection_press`'s `close` are — and
    /// read afresh on every event rather than snapshotted at the press, because
    /// the wheel still zooms while a polygon is half drawn.
    fn lasso_step(&self) -> f32 {
        SELECT_SAMPLE_PIXELS / self.camera.zoom.max(1e-3)
    }

    pub fn selection_moved(&mut self, doc: Vec2) {
        let step = self.lasso_step();
        if let Some(draft) = self.selection_draft.as_mut() {
            draft.moved(doc, step);
        }
    }

    pub fn selection_release(&mut self, doc: Vec2) {
        let step = self.lasso_step();
        let Some(draft) = self.selection_draft.as_mut() else {
            // The draft went while the button was down — Escape, or a tool
            // shortcut. The button coming up is then what ends the gesture,
            // and leaving the interaction in `Selecting` would leave it with
            // nothing that could ever end it.
            self.interaction = Interaction::Idle;
            return;
        };
        if draft.release(doc, step) {
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

impl Editor {
    /// Is a pick *aimed* — the eyedropper in hand, over somewhere a press would
    /// actually take a colour from?
    ///
    /// One function because there are two things that must agree about it and
    /// they are in different modules, which is [`over_egui_area`]'s own reason:
    /// `ui::aiming_cursor` draws the crosshair here, and `App::pick_aimed`
    /// reads a pixel and shows the loupe here. Two copies would be a magnifier
    /// promising a colour where the crosshair is not, or the reverse.
    ///
    /// **It is deliberately the canvas alone, even though a *drag* now picks
    /// off the interface too.** A press over a docked panel operates the panel
    /// — those are controls, and an eyedropper does not get to take the Layers
    /// panel's eye toggle away — so a loupe hovering there would be offering a
    /// colour that clicking will not take, which is exactly the control that
    /// lies this project refuses everywhere. Once a drag is in flight
    /// `Interaction::Picking` answers instead and the interface is read like
    /// anything else; that is the gesture the artist's report was about.
    ///
    /// Alt with another tool in hand is not aimed either: Alt with no button is
    /// the brush resize, so a loupe there would be a second reading of a
    /// modifier that already means something.
    ///
    /// **And no other gesture may be in flight**, which the first draft left
    /// out and which is worse than it sounds. A middle-drag pans with *any*
    /// tool selected, so panning with the eyedropper in hand answered "aimed":
    /// a loupe promising a colour over a canvas sliding under the pointer, that
    /// a release would not take — and a blocking GPU readback inserted into
    /// every frame of a gesture that had cost nothing. [`Interaction::allows_
    /// aim`] is the reading, and it is an exhaustive `match` rather than a
    /// `matches!` for the reason CLAUDE.md gives: a new interaction has to
    /// decide.
    pub fn aiming_pick(&self, around: Surroundings) -> bool {
        self.ui.tool == Tool::Eyedropper
            && self.interaction.allows_aim()
            && !around.over_area
            && around.focused
            && self.pointer_over_canvas(self.cursor)
    }
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
    fn a_pick_is_only_aimed_when_no_other_gesture_holds_the_pointer() {
        // **A middle-drag pans with any tool selected**, so this is not an
        // exotic combination: it is what happens when somebody with the
        // eyedropper in hand shoves the canvas along to see the rest of it.
        // Answering "aimed" there drew a loupe over a picture sliding under the
        // pointer, promising a colour the release would not take — and paid a
        // blocking GPU readback on every frame of a gesture that had cost
        // nothing. The first draft of `aiming_pick` did not read `interaction`
        // at all.
        let mut ed = windowed();
        ed.ui.tool = Tool::Eyedropper;
        ed.cursor = Vec2::new(500.0, 400.0);
        assert!(
            ed.pointer_over_canvas(ed.cursor),
            "the fixture aims at canvas"
        );

        assert!(ed.aiming_pick(clear()), "idle over the canvas is the hover");
        ed.interaction = Interaction::Picking;
        assert!(
            ed.aiming_pick(clear()),
            "and the drag itself keeps the crosshair"
        );
        for busy in [
            Interaction::Drawing,
            Interaction::Selecting,
            Interaction::Panning,
            Interaction::Zooming,
        ] {
            ed.interaction = busy;
            assert!(!ed.aiming_pick(clear()), "{busy:?} is another gesture");
        }
    }

    #[test]
    fn nothing_but_the_eyedropper_over_the_canvas_aims_a_pick() {
        // The other three clauses, each on its own, because a conjunction is
        // only tested by cases where one term is false and the rest are true.
        let mut ed = windowed();
        ed.ui.tool = Tool::Eyedropper;
        ed.cursor = Vec2::new(500.0, 400.0);
        assert!(ed.aiming_pick(clear()));

        // A menu, a dropdown or a modal over the pointer: the ordinary cursor
        // is the right one and nothing is read.
        assert!(!ed.aiming_pick(Surroundings {
            over_area: true,
            focused: true
        }));
        // Another application has the keyboard.
        assert!(!ed.aiming_pick(Surroundings {
            over_area: false,
            focused: false
        }));
        // Over a panel. A press there operates the panel, so a loupe would be
        // offering a colour clicking will not take — the drag reaches the
        // interface through `Interaction::Picking` instead.
        ed.cursor = Vec2::new(500.0, 5.0);
        assert!(
            !ed.pointer_over_canvas(ed.cursor),
            "the menu bar, not canvas"
        );
        assert!(!ed.aiming_pick(clear()));
        // And any other tool. Alt with a brush in hand is the brush resize.
        ed.cursor = Vec2::new(500.0, 400.0);
        ed.ui.tool = Tool::Brush;
        assert!(!ed.aiming_pick(clear()));
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

    /// A lock refuses a step over a canvas flip, in **both** directions, and
    /// refuses nothing else.
    ///
    /// Driven over the whole of [`umber_core::EditKind::ALL`] rather than over
    /// the two flips, because the interesting failure is the *other* half: a
    /// gate that refused every kind while a layer was locked would make Ctrl+Z
    /// inert for a painter who had locked a reference layer, which is an
    /// ordinary thing to have done, and no test that only drove the flips could
    /// tell the two rules apart. Every non-flip kind is asserted `Clear` with a
    /// layer locked.
    ///
    /// Both stacks, because a flip is its own inverse and a redo that spent an
    /// entry it could not carry out damages the document exactly as an undo
    /// does. The redo half is set up by recording and then taking the entry, so
    /// the fixture reaches that stack the way the application does.
    ///
    /// **What this does not cover**: whether `App::undo` consults the gate at
    /// all. `App` holds a `winit::Window` and a `wgpu::Surface`, so it cannot be
    /// built headlessly and there is no test in this crate that drives it. What
    /// stands in for one is structural rather than a guard —
    /// `App::mirror_document` is `#[must_use]`, so the defect this gate exists
    /// for (a discarded refusal) is a compile error under CI's `-D warnings` —
    /// and the panel half is
    /// `crate::ui::tests::the_edit_menus_history_rows_go_dead_when_a_lock_
    /// refuses_the_flip`, which clicks the real row.
    #[test]
    fn a_lock_refuses_a_step_over_a_flip_and_over_nothing_else() {
        for kind in umber_core::EditKind::ALL {
            let flip = kind.flip_axis().is_some();
            // The body is `Flip` for every kind, which is not a state the
            // history produces and is deliberate here: the gate is supposed to
            // read the **kind**, so a fixture whose body agreed with the kind
            // could not tell a gate that read the body from one that read the
            // kind. `App::reverse` reads the body and the kind separately, and
            // only the kind decides whether a mirror is about to happen.
            let mut ed = Editor::default();
            ed.history
                .record(umber_core::Edit::new(kind, umber_core::EditBody::Flip));

            assert_eq!(
                ed.undo_gate(),
                if flip {
                    StepGate::SettleForFlip
                } else {
                    StepGate::Clear
                },
                "unlocked, undo over {kind:?}"
            );

            ed.layers.active_mut().locked = true;
            assert_eq!(
                ed.undo_gate(),
                if flip {
                    StepGate::FlipLocked
                } else {
                    StepGate::Clear
                },
                "locked, undo over {kind:?}"
            );

            // Onto the redo stack the way the application puts it there.
            ed.layers.active_mut().locked = false;
            let edit = ed.history.take_undo().expect("the entry just recorded");
            ed.history.push_redo(edit);
            assert_eq!(
                ed.redo_gate(),
                if flip {
                    StepGate::SettleForFlip
                } else {
                    StepGate::Clear
                },
                "unlocked, redo over {kind:?}"
            );

            ed.layers.active_mut().locked = true;
            assert_eq!(
                ed.redo_gate(),
                if flip {
                    StepGate::FlipLocked
                } else {
                    StepGate::Clear
                },
                "locked, redo over {kind:?}"
            );
        }

        // An empty stack is `Clear` and not a fourth answer: `take_undo` says
        // "nothing" a line later, and there is nothing to tell anybody about.
        let mut ed = Editor::default();
        ed.layers.active_mut().locked = true;
        assert_eq!(ed.undo_gate(), StepGate::Clear, "nothing to undo");
        assert_eq!(ed.redo_gate(), StepGate::Clear, "nothing to redo");

        // **Which end of the stack the gate reads**, which every case above is
        // blind to because none of them holds more than one entry. A critic
        // pointed this out and the mutation was run: turning
        // `History::next_undo` into `self.undo.first()` left every assertion
        // above green, and reproduces the original bug exactly — the gate would
        // read the Paint at the bottom, answer `Clear`, and let the flip through
        // unmirrored.
        //
        // **The two stacks are loaded with opposite contents on purpose.** Made
        // symmetric they were, and a `redo_gate` that read the undo stack passed
        // every assertion in this test and in the menu's. Here each direction
        // is asked over a stack whose top disagrees with the other's, so the two
        // cannot be swapped without one of the four answers moving.
        let mut ed = Editor::default();
        ed.layers.active_mut().locked = true;
        // Undo: [Paint, Flip] with the flip on top. Redo: [Flip, Paint], paint
        // on top. So undo must refuse and redo must not.
        for kind in [
            umber_core::EditKind::Paint,
            umber_core::EditKind::FlipVertical,
        ] {
            ed.history
                .record(umber_core::Edit::new(kind, umber_core::EditBody::Flip));
        }
        for kind in [
            umber_core::EditKind::FlipVertical,
            umber_core::EditKind::Paint,
        ] {
            ed.history
                .push_redo(umber_core::Edit::new(kind, umber_core::EditBody::Flip));
        }
        // **The fixture is checked before it is used**, because its own
        // ordering is load-bearing and silently so: `History::record` drains
        // the redo stack, so putting the two loops the other way round leaves
        // redo *empty*, and both assertions below then pass on `None => Clear`
        // having tested nothing. A second critic asked for that mutation and it
        // was green. Two entries applied and four held is the shape the four
        // answers below mean anything over.
        assert_eq!(ed.history.position(), 2, "the undo stack is not as loaded");
        assert_eq!(ed.history.len(), 4, "the redo stack was drained");
        assert_eq!(
            ed.undo_gate(),
            StepGate::FlipLocked,
            "the gate read past the flip on top of the undo stack"
        );
        assert_eq!(
            ed.redo_gate(),
            StepGate::Clear,
            "the gate read past the paint on top of the redo stack"
        );
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

    /// A text record for a layer of a default-sized document.
    fn a_record(caption: &str) -> umber_core::textobj::TextObject {
        use umber_core::text::{Align, TextBlock};
        use umber_core::textobj::{Placement, TextFace, TextObject};
        TextObject::new(
            TextBlock {
                text: caption.to_string(),
                size: 48.0,
                line_spacing: 1.2,
                tracking: 0.0,
                align: Align::Left,
            },
            TextFace {
                family: "Archivo".into(),
                style: "Regular".into(),
                postscript: String::new(),
            },
            Color::new(0.1, 0.1, 0.1, 1.0),
            Placement::identity(umber_core::PixelRect {
                x: 10,
                y: 20,
                width: 100,
                height: 40,
            }),
        )
    }

    /// **A text layer refuses a brush, and its mask does not.**
    ///
    /// The gate is `LayerStack::refusal_at` and this is the *call site*, which
    /// is the half a test of the model cannot see: the model answered `Text`
    /// for months while `begin_stroke` read `active_is_locked` and never asked.
    /// A stroke landing on a text layer leaves the record describing pixels it
    /// did not make, and the fingerprint does not cover for that — a save
    /// fingerprints the pixels it is writing, so the file agrees with itself
    /// and the reader re-renders over somebody's brushwork.
    ///
    /// The mask half is not a nicety. A mask bounds the alpha the composite
    /// reads and changes none of the layer's own pixels, so it cannot put the
    /// record out of step with them — and refusing it would take a working
    /// control away for no reason.
    #[test]
    fn a_text_layer_refuses_a_stroke_and_its_mask_still_takes_one() {
        let mut ed = Editor::default();
        assert!(ed.begin_stroke(point()), "an ordinary layer paints");
        ed.stroke.end();
        ed.interaction = Interaction::Idle;

        assert!(ed.layers.set_text(0, a_record("A caption")));
        assert!(
            !ed.begin_stroke(point()),
            "a brush reached a text layer's own pixels"
        );
        assert_eq!(
            ed.interaction,
            Interaction::Idle,
            "a refused stroke must not leave the pointer drawing"
        );
        assert!(!ed.stroke.is_active());

        // The mask is a different slice and cannot disagree with the record.
        let mask = ed.layers.add_mask(0).expect("a mask");
        ed.edit_target = EditTarget::Mask;
        assert!(ed.begin_stroke(point()), "a text layer's mask must paint");
        assert_eq!(ed.stroke_slot, mask);

        // **And the fallback is the case that would have been silent.** With
        // the strip on Mask and the mask taken off, `stroke_target` falls back
        // to the *layer* — so asking about the target the strip names rather
        // than the one it resolved to would let a brush straight through.
        ed.stroke.end();
        ed.interaction = Interaction::Idle;
        assert!(ed.layers.remove_mask(0).is_some());
        assert_eq!(
            ed.stroke_target(),
            ed.layers.active_slot().map(|s| (s, false))
        );
        assert!(
            !ed.begin_stroke(point()),
            "the mask fell back to the layer and the layer took the stroke"
        );

        // Taking the record off is what lets paint back on, which is the whole
        // of what "Convert to paint" does.
        assert!(ed.layers.take_text(0).is_some());
        ed.edit_target = EditTarget::Layer;
        assert!(ed.begin_stroke(point()));
    }

    /// **A canvas flip mirrors the record with the pixels**, at the one place a
    /// flip reaches the model.
    ///
    /// `Editor::flip_canvas` is called for the flip and again for its undo, so
    /// a record left behind would put the next re-render where the text used to
    /// be — un-mirroring the layer against a picture that had turned over. The
    /// mirror is exact, which is what lets a flip keep the record where a
    /// resize drops it: undoing a flip is another flip.
    #[test]
    fn a_canvas_flip_mirrors_a_text_layers_placement() {
        use umber_core::FlipAxis;
        let mut ed = Editor::default();
        let before = a_record("A caption");
        assert!(ed.layers.set_text(0, before.clone()));
        let canvas = ed.doc.size;

        ed.flip_canvas(FlipAxis::Horizontal);
        let flipped = ed.layers.text_at(0).expect("the record survived").clone();
        assert_ne!(
            flipped.placement, before.placement,
            "the record was not mirrored at all"
        );
        assert_eq!(
            flipped.placement,
            before
                .placement
                .flipped(FlipAxis::Horizontal, canvas)
                .expect("inside the canvas")
        );

        // Undoing a flip is another flip, and it has to be exact.
        ed.flip_canvas(FlipAxis::Horizontal);
        assert_eq!(
            ed.layers.text_at(0).expect("still a record").placement,
            before.placement
        );
        assert!(ed.notice.is_none(), "nothing was lost, so nothing is said");
    }

    /// **A resize takes every record off and says how many**, for the reason it
    /// clears the history: a placement is a rectangle of a canvas that no longer
    /// exists, and a canvas that shrank has cropped the pixels the record
    /// describes.
    ///
    /// A resize that changes nothing changes nothing, which is the case a
    /// `drop_text_objects` called unconditionally would get wrong — pressing
    /// Apply on a dialog nobody touched would quietly make every caption in the
    /// document paint.
    #[test]
    fn resizing_a_document_takes_its_text_records_off_and_names_the_loss() {
        let mut ed = Editor::default();
        assert!(ed.layers.set_text(0, a_record("A caption")));

        // The same size is not a resize.
        let same = ed.doc;
        assert!(!ed.apply_canvas(same));
        assert!(
            ed.layers.text_at(0).is_some(),
            "an untouched dialog dropped it"
        );
        assert!(ed.notice.is_none());

        let bigger = Document::new(ed.doc.size.x + 64, ed.doc.size.y);
        assert!(ed.apply_canvas(bigger));
        assert!(
            ed.layers.text_at(0).is_none(),
            "a resized document kept a placement of the canvas it no longer has"
        );
        let notice = ed.notice.as_ref().expect("the loss was silent");
        assert!(
            notice.lines[0].contains("one text layer"),
            "{:?}",
            notice.lines
        );
        // A bracketed plural is a thing a program writes, not a thing a person
        // does, and everything here is read by somebody who was painting.
        assert!(!notice.lines[0].contains("(s)"), "{:?}", notice.lines);
        assert_eq!(text_layers(1), "one text layer");
        assert_eq!(text_layers(3), "3 text layers");
        assert!(
            !notice.lines[0].contains('—'),
            "no em-dash in a notice: {:?}",
            notice.lines
        );
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
    /// is the gap `LayerStack::flip_text`'s own docs used to name in as many
    /// words — "nothing calls it yet" — and the one an effect's flip had no note
    /// about at all, which is why a second agent had to find it. A test at the
    /// call site is worth more than a note, so here is one; the text record's
    /// own is `a_canvas_flip_mirrors_a_text_layers_placement`.
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

    // --- what the options strip's settings actually reach ------------------

    /// One rectangular gesture, made the way the pointer makes one.
    fn drag_a_box(ed: &mut Editor, a: Vec2, b: Vec2) {
        ed.selection_press(a, SelectionOp::Replace);
        ed.selection_moved(b);
        ed.selection_release(b);
    }

    /// **Every setting on the Select strip reaches the selection a gesture
    /// makes**, which is the half `umber-core`'s own guards cannot see.
    ///
    /// `SelectionDraft`'s tests drive the draft directly, so all four would
    /// stay green if `selection_press` stopped reading one of these fields —
    /// the `docs` rule that a guard on a model is not a guard on the panel,
    /// one level down from the panel. Each is measured off the finished
    /// `Selection` rather than read back off the draft.
    ///
    /// **The feather is here because it was reported as doing nothing**, and
    /// this is the evidence that it does: a rectangle drawn with the rail at
    /// four softens outwards over a band eight pixels wide, and the same
    /// rectangle with the rail at zero is hard at its own edge. What the artist
    /// could not see is real and is not this: the *marquee* is the rings, and
    /// the rings are deliberately left exactly where they were, so a feathered
    /// selection looks identical to a sharp one until something is painted
    /// through it.
    #[test]
    fn every_setting_on_the_select_strip_reaches_the_selection_it_makes() {
        let mut ed = Editor::default();
        ed.ui.tool = Tool::Select;

        // Feather. A pixel outside the box is untouched by a sharp gesture and
        // partly selected by a soft one, and the soft one's own edge has come
        // down off full coverage.
        ed.ui.selection_mode = SelectionMode::Rectangle;
        drag_a_box(&mut ed, Vec2::new(20.0, 20.0), Vec2::new(60.0, 60.0));
        let sharp = ed.selection.clone().expect("a rectangle");
        assert_eq!(sharp.feather(), 0.0);
        assert_eq!(sharp.coverage_at(19, 40), 0);
        assert_eq!(sharp.coverage_at(20, 40), 255);

        ed.ui.selection_feather = 4.0;
        drag_a_box(&mut ed, Vec2::new(20.0, 20.0), Vec2::new(60.0, 60.0));
        let soft = ed.selection.clone().expect("a soft rectangle");
        assert_eq!(soft.feather(), 4.0, "the rail never reached the gesture");
        assert!(
            soft.coverage_at(19, 40) > 0,
            "the edge did not soften outwards"
        );
        assert!(
            soft.coverage_at(20, 40) < 255,
            "the edge did not soften inwards"
        );
        assert!(soft.bounds().x < sharp.bounds().x, "the box did not grow");
        ed.ui.selection_feather = 0.0;

        // Roundness. The corner of the box is inside a square selection and
        // outside a rounded one.
        assert_eq!(sharp.coverage_at(20, 20), 255);
        ed.ui.selection_roundness = 1.0;
        drag_a_box(&mut ed, Vec2::new(20.0, 20.0), Vec2::new(60.0, 60.0));
        let round = ed.selection.clone().expect("a disc");
        assert_eq!(
            round.coverage_at(20, 20),
            0,
            "the roundness rail never reached the gesture"
        );
        assert_eq!(round.coverage_at(40, 40), 255, "the middle went missing");
        ed.ui.selection_roundness = 0.0;

        // Stabiliser. The same freehand path, with and without: damped, the
        // recorded outline never reaches the corner the hand went round.
        ed.ui.selection_mode = SelectionMode::Lasso;
        let corner = Vec2::new(60.0, 20.0);
        // Fifteen reports a leg, so each is about two and a half pixels — which
        // is what a hand moving at any speed a stabiliser is for actually
        // produces, and coarse enough that the filter's four-report lag is
        // pixels rather than fractions of one.
        let path = |ed: &mut Editor| {
            ed.selection_press(Vec2::new(20.0, 20.0), SelectionOp::Replace);
            for k in 1..=15 {
                ed.selection_moved(Vec2::new(20.0, 20.0).lerp(corner, k as f32 / 15.0));
            }
            for k in 1..=15 {
                ed.selection_moved(corner.lerp(Vec2::new(60.0, 60.0), k as f32 / 15.0));
            }
            ed.selection_moved(Vec2::new(20.0, 60.0));
            ed.selection_release(Vec2::new(20.0, 60.0));
            ed.selection.clone().expect("a lasso")
        };
        let loose = path(&mut ed);
        ed.ui.selection_stabiliser = 0.8;
        let damped = path(&mut ed);
        let nearest = |s: &Selection| {
            s.rings()[0]
                .iter()
                .map(|p| p.distance(corner))
                .fold(f32::MAX, f32::min)
        };
        assert!(
            nearest(&loose) < 1.0,
            "the raw outline did not go round the corner"
        );
        assert!(
            nearest(&damped) > 2.5,
            "the stabiliser rail never reached the gesture: the outline came \
             within {:.1} px of the corner",
            nearest(&damped)
        );
    }

    /// The lasso's sampling step follows the camera, which is half of the fix
    /// for the reported staircase — the half that decides how much of the hand
    /// is recorded in the first place.
    ///
    /// **One document path, drawn at two zooms.** That is what isolates the
    /// step: the pointer reports the same positions either way, so any
    /// difference in what comes back is `selection_moved` having asked the
    /// camera. Reports a third of a pixel apart, which the old constant
    /// document pixel threw two out of every three of away at any zoom, and
    /// which 4:1 keeps whole.
    ///
    /// The vertex count is what is measured, and the bound is deliberately only
    /// "more" rather than a ratio, because the two halves of the fix pull
    /// against each other here: the 1:1 gesture keeps a third of the reports
    /// and is then subdivided once for having pixel-wide gaps, where the 4:1
    /// one keeps all of them and needs no subdivision at all. Measured, that is
    /// 481 vertices against 314 rather than the threefold difference the
    /// sampling alone would give. What the comparison still cannot be fooled by
    /// is a step that ignores the camera, which would make the two equal.
    ///
    /// It also pins where the reading happens. A step worked out at the press
    /// and snapshotted would pass a test that never moved the camera, and would
    /// be wrong the moment somebody rolled the wheel with a polygon half drawn.
    #[test]
    fn the_lasso_records_a_finer_line_the_closer_the_camera_is() {
        let corners = [
            Vec2::new(20.0, 20.0),
            Vec2::new(60.0, 20.0),
            Vec2::new(60.0, 60.0),
            Vec2::new(20.0, 60.0),
        ];
        let mut path = Vec::new();
        for leg in [
            (corners[0], corners[1]),
            (corners[1], corners[2]),
            (corners[2], corners[3]),
            (corners[3], corners[0]),
        ] {
            let steps = 120; // 40 document pixels in thirds of one.
            for k in 1..=steps {
                path.push(leg.0.lerp(leg.1, k as f32 / steps as f32));
            }
        }

        let recorded_at = |zoom: f32| {
            let mut ed = Editor::default();
            ed.ui.tool = Tool::Select;
            ed.ui.selection_mode = SelectionMode::Lasso;
            ed.camera.zoom = zoom;
            ed.selection_press(corners[0], SelectionOp::Replace);
            for p in &path {
                ed.selection_moved(*p);
            }
            ed.selection_release(*path.last().expect("a path"));
            ed.selection.clone().expect("a lasso").rings()[0].len()
        };
        let coarse = recorded_at(1.0);
        let fine = recorded_at(4.0);
        assert!(
            fine > coarse,
            "the same path recorded {fine} vertices at 4:1 and {coarse} at 1:1, \
             which is not the step following the camera"
        );
    }
    /// A placement makes its layer, records nothing, and gets its one entry at
    /// the commit.
    ///
    /// **Driven through `make_text_layer` and `commit_made_layer` rather than
    /// built from their parts**, which is the whole reason both are on `Editor`:
    /// a critic deleted the recording when it lived at the `App` call site and
    /// all 856 tests stayed green, because every guard here assembled the entry
    /// itself. A placement with no undo entry at all is one Ctrl+Z cannot take
    /// back.
    ///
    /// Three readings. The layer appears and is named after its words; the
    /// history does **not** move while the box is in the air, which is the whole
    /// of why Escape is free; and the entry the commit records really does take
    /// the layer back out when it is restored, which is the reading that catches
    /// a shape snapshotted after the add instead of before it.
    #[test]
    fn a_placement_records_nothing_until_it_commits() {
        let mut ed = Editor::default();
        let position = ed.history.position();

        let made = ed.make_text_layer("Chapter One").expect("room for one");
        assert_eq!(ed.layers.len(), 2);
        assert_eq!(
            ed.layers.get(ed.layers.active_index()).unwrap().name,
            "Chapter One",
            "the layer is not named after its words"
        );
        assert_eq!(
            ed.history.position(),
            position,
            "the placement recorded an entry while the box was still in the air, \
             so Escape has a redo stack to drain"
        );

        let id = made.id;
        ed.commit_made_layer(made);
        assert_eq!(
            ed.history.position(),
            position + 1,
            "the commit recorded no entry, so one Ctrl+Z cannot take it back"
        );
        let entry = ed.history.take_undo().expect("the placement");
        assert_eq!(entry.kind, umber_core::EditKind::AddLayer);
        let umber_core::EditBody::Structure(shape) = entry.body else {
            panic!("a placement records a shape");
        };
        ed.layers.restore_shape(*shape);
        assert!(
            !ed.layers.layers().iter().any(|l| l.id() == id),
            "undoing the placement left its layer in the stack, which means the \
             shape was snapshotted after the add rather than before it"
        );
    }

    /// **Escape after a placement leaves the redo stack exactly as it found
    /// it.**
    ///
    /// The headline property, and the one the entry moved back to the commit
    /// for. `History::record` drains the redo stack and nothing can put it back,
    /// so while the placement recorded at the *add* this sequence — undo an
    /// edit, place a caption, change your mind — cost the artist the edit they
    /// had undone, silently, for a gesture that changed nothing.
    ///
    /// It measures the redo stack rather than asserting that nothing recorded:
    /// what an artist loses is the ability to press Ctrl+Y, so the guard takes
    /// the entry back off and checks it still does what it said.
    #[test]
    fn escaping_a_placement_leaves_the_redo_stack_it_found() {
        let mut ed = Editor::default();
        // Something to have undone. A second layer added and taken back off is
        // the cheapest edit this module can make with no device at all.
        let before = ed.layers.shape(ed.doc.layer_bytes());
        ed.layers.add().expect("a second layer");
        let added = ed.layers.get(ed.layers.active_index()).unwrap().id();
        ed.history.record(umber_core::Edit::new(
            umber_core::EditKind::AddLayer,
            before,
        ));
        let entry = ed.history.take_undo().expect("the add");
        let umber_core::EditBody::Structure(shape) = entry.body else {
            panic!("an add records a shape");
        };
        let inverse = ed.layers.restore_shape(*shape);
        ed.history
            .push_redo(umber_core::Edit::new(entry.kind, inverse));
        assert_eq!(ed.history.len() - ed.history.position(), 1, "one to redo");

        // Place a caption and change your mind.
        let made = ed.make_text_layer("Caption").expect("room for one");
        ed.unmake_layer(Some(made));

        assert_eq!(
            ed.history.len() - ed.history.position(),
            1,
            "pressing Escape threw away the edit the artist had undone"
        );
        let redo = ed.history.take_redo().expect("the add, still redoable");
        let umber_core::EditBody::Structure(shape) = redo.body else {
            panic!("an add records a shape");
        };
        ed.layers.restore_shape(*shape);
        assert!(
            ed.layers.layers().iter().any(|l| l.id() == added),
            "the redo stack survived but no longer redoes anything"
        );
    }

    /// Abandoning a placement takes back its layer and its slice, and leaves the
    /// history alone.
    ///
    /// Three readings, each catching a different mistake. The **stack** comes
    /// back to its length, which any implementation gets right. The **slot pool**
    /// is what sees a slice parked rather than released: parking is for a layer
    /// an undo entry could put back, and there is no entry here at all, so it
    /// would leak a canvas-sized slice for pressing Escape with nothing about the
    /// stack saying so. And the **history** is untouched — see
    /// `escaping_a_placement_leaves_the_redo_stack_it_found` for the half of that
    /// which costs an artist something.
    #[test]
    fn abandoning_a_placement_takes_back_its_layer_and_its_slice() {
        let mut ed = Editor::default();
        let was_active = ed.layers.get(ed.layers.active_index()).unwrap().id();
        let ceiling = ed.layers.live_slot_ceiling();
        let position = ed.history.position();

        let made = ed.make_text_layer("Caption").expect("room for one");
        assert!(ed.layers.live_slot_ceiling() > ceiling, "it took a slice");

        ed.unmake_layer(Some(made));
        assert_eq!(ed.layers.len(), 1, "the layer went");
        assert_eq!(
            ed.layers.live_slot_ceiling(),
            ceiling,
            "the slice was parked rather than given back"
        );
        assert_eq!(
            ed.history.position(),
            position,
            "an entry was left behind that would undo a layer that has gone"
        );
        assert_eq!(
            ed.layers.get(ed.layers.active_index()).unwrap().id(),
            was_active,
            "the selection went back where the artist left it"
        );
    }

    /// **A reorder made before a placement undoes after it, in the order the
    /// artist made them.**
    ///
    /// This is the sequence a critic found, and it is why every route to a
    /// reorder now settles the float first: `App::record_move` for both chevrons
    /// and, through `UiActions::reorder_layer`, for the Layers panel's drag. With
    /// that in place a `MoveLayer` cannot land in the middle of a placement at
    /// all, so the placement's entry is safe to record at its commit — which is
    /// what makes Escape free.
    ///
    /// Both undos are carried out rather than asserted about, and what is
    /// measured is that each one actually **moved the stack** rather than being
    /// handed back unchanged. What no test here can see is whether
    /// `record_move` calls `finish_transform`; that is `app.rs`'s source scan,
    /// which says so too.
    ///
    /// **The opposite order cannot be driven at all, and that is worth knowing
    /// before somebody tries.** A shape naming a layer that has gone reaches
    /// `LayerStack::restore_shape`'s `debug_assert!(false, ...)`, so under
    /// `cargo test` the bad order panics rather than producing the silent
    /// no-undo it produces in a release build. The refusal is stated where it
    /// lives; a guard here could only wrap it in `catch_unwind` and would then
    /// be a test of `debug_assertions`.
    #[test]
    fn a_reorder_before_a_placement_undoes_after_it() {
        let mut ed = Editor::default();
        ed.layers.add().expect("a second layer");
        let bottom = ed.layers.get(0).unwrap().id();

        // The drag settles the float, so the reorder is recorded first.
        let move_before = ed.layers.shape(ed.doc.layer_bytes());
        assert!(ed.layers.reorder_to(0, 1, 0), "the drag moved something");
        ed.history.record(umber_core::Edit::new(
            umber_core::EditKind::MoveLayer,
            move_before,
        ));
        assert_ne!(
            ed.layers.get(0).unwrap().id(),
            bottom,
            "the fixture did not reorder anything"
        );

        // Then the placement, recorded at its commit.
        let made = ed.make_text_layer("Caption").expect("a third");
        let id = made.id;
        let slot = ed
            .layers
            .layers()
            .iter()
            .find(|l| l.id() == id)
            .and_then(|l| l.slot())
            .expect("the new layer took a slice");
        ed.commit_made_layer(made);

        // Ctrl+Z, twice, newest first — which is the order `App::undo` walks.
        let add_entry = ed.history.take_undo().expect("the placement");
        assert_eq!(add_entry.kind, umber_core::EditKind::AddLayer);
        let umber_core::EditBody::Structure(shape) = add_entry.body else {
            panic!("a placement records a shape");
        };
        let redo = ed.layers.restore_shape(*shape);
        assert!(
            !ed.layers.layers().iter().any(|l| l.id() == id),
            "the first undo did not take the caption's layer out"
        );
        let move_entry = ed.history.take_undo().expect("the reorder");
        let umber_core::EditBody::Structure(shape) = move_entry.body else {
            panic!("a reorder records a shape");
        };
        ed.layers.restore_shape(*shape);
        assert_eq!(
            ed.layers.get(0).unwrap().id(),
            bottom,
            "the reorder's undo was refused: the history moved and the picture did not"
        );

        // And redo puts the layer back with its slice, which is what makes the
        // pixels the commit wrote come back with it.
        ed.layers.restore_shape(redo);
        let at = ed
            .layers
            .layers()
            .iter()
            .position(|l| l.id() == id)
            .expect("the layer came back");
        assert_eq!(ed.layers.get(at).unwrap().slot(), Some(slot));
        assert_eq!(ed.layers.get(at).unwrap().name, "Caption");
    }

    /// Nothing was made, so nothing is taken back. The case every ordinary
    /// paste and every lift goes down.
    #[test]
    fn a_float_that_made_no_layer_takes_none_away() {
        let mut ed = Editor::default();
        ed.layers.add().expect("a second layer");
        let before: Vec<u32> = ed.layers.layers().iter().map(|l| l.id()).collect();
        ed.unmake_layer(None);
        let after: Vec<u32> = ed.layers.layers().iter().map(|l| l.id()).collect();
        assert_eq!(before, after);
    }
}
