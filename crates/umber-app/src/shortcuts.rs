//! Keyboard shortcuts as data.
//!
//! The bindings live in a table rather than in a `match` so the settings dialog
//! can list them: a match arm can be executed but not enumerated, printed or
//! rebound. [`resolve`] turns a key press back into an [`Action`], and the event
//! loop then does exactly what it did before.

use std::sync::RwLock;
use winit::keyboard::{KeyCode, ModifiersState};

/// Something the user can bind a key to.
///
/// One variant per distinct command, *not* per binding — Redo has two default
/// bindings and is still one action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Save,
    SaveAs,
    Export,
    Undo,
    Redo,
    Deselect,
    Copy,
    Cut,
    Paste,
    FlipCanvasHorizontal,
    FlipCanvasVertical,
    BrushTool,
    EraserTool,
    SelectTool,
    TransformTool,
    PanTool,
    ZoomTool,
    SizeDown,
    SizeUp,
    // The temporary brush changes. Every one of these is unbound by default and
    // bindable, which is the whole of why fourteen of them can exist at all: a
    // set of keys nobody asked for would collide with whatever the painter's
    // hands already know, and choosing well for everybody across six settings
    // is not possible. What each one *does* is `tweaks::of_action`'s — this
    // table stays a plain list of bindings.
    OpacityDown,
    OpacityUp,
    HardnessDown,
    HardnessUp,
    SpacingDown,
    SpacingUp,
    RoundnessDown,
    RoundnessUp,
    AirbrushDown,
    AirbrushUp,
    AngleDown,
    AngleUp,
    PickupDown,
    PickupUp,
    SwapColours,
    FitView,
    ActualSize,
    ZoomIn,
    ZoomOut,
}

impl Action {
    /// Every action, in the order the settings dialog lists them.
    ///
    /// Walking this rather than `defaults()` means an action with no binding
    /// still appears — shown as unbound — instead of silently vanishing from
    /// the list the moment someone forgets to bind it.
    pub const ALL: [Action; 38] = [
        Action::Save,
        Action::SaveAs,
        Action::Export,
        Action::Undo,
        Action::Redo,
        Action::Deselect,
        Action::Copy,
        Action::Cut,
        Action::Paste,
        Action::FlipCanvasHorizontal,
        Action::FlipCanvasVertical,
        Action::BrushTool,
        Action::EraserTool,
        Action::SelectTool,
        Action::TransformTool,
        Action::PanTool,
        Action::ZoomTool,
        Action::SizeDown,
        Action::SizeUp,
        // Contiguous with the pair above, because the settings list draws a
        // category heading whenever the category changes as it walks this
        // array — so a group split in two would draw "Brush" twice with
        // something else between. `the_brush_actions_are_contiguous_in_the_
        // settings_list` guards it.
        Action::OpacityDown,
        Action::OpacityUp,
        Action::HardnessDown,
        Action::HardnessUp,
        Action::SpacingDown,
        Action::SpacingUp,
        Action::RoundnessDown,
        Action::RoundnessUp,
        Action::AirbrushDown,
        Action::AirbrushUp,
        Action::AngleDown,
        Action::AngleUp,
        Action::PickupDown,
        Action::PickupUp,
        Action::SwapColours,
        Action::FitView,
        Action::ActualSize,
        Action::ZoomIn,
        Action::ZoomOut,
    ];

    /// Human-readable, British spelling, e.g. "Swap colours".
    pub fn label(self) -> &'static str {
        match self {
            Action::Save => "Save",
            Action::SaveAs => "Save as…",
            Action::Export => "Export image…",
            Action::Undo => "Undo",
            Action::Redo => "Redo",
            Action::Deselect => "Deselect",
            Action::Copy => "Copy",
            Action::Cut => "Cut",
            Action::Paste => "Paste",
            Action::FlipCanvasHorizontal => "Flip canvas horizontally",
            Action::FlipCanvasVertical => "Flip canvas vertically",
            Action::BrushTool => "Brush tool",
            Action::EraserTool => "Eraser tool",
            Action::SelectTool => "Selection tool",
            Action::TransformTool => "Transform tool",
            Action::PanTool => "Pan tool",
            Action::ZoomTool => "Zoom tool",
            Action::SizeDown => "Decrease brush size",
            Action::SizeUp => "Increase brush size",
            // The wording is "the brush", singular and definite, because every
            // one of these changes the brush *in hand* and not the brush that
            // is saved — see `tweaks`. The labels are also what the search
            // field matches on, so each names the setting the way the rail
            // does rather than the way the engine's field is spelt.
            Action::OpacityDown => "Decrease brush opacity",
            Action::OpacityUp => "Increase brush opacity",
            Action::HardnessDown => "Decrease brush hardness",
            Action::HardnessUp => "Increase brush hardness",
            Action::SpacingDown => "Decrease brush spacing",
            Action::SpacingUp => "Increase brush spacing",
            Action::RoundnessDown => "Decrease brush roundness",
            Action::RoundnessUp => "Increase brush roundness",
            Action::AirbrushDown => "Decrease airbrush rate",
            Action::AirbrushUp => "Increase airbrush rate",
            // Named for the number rather than for a direction on screen: the
            // angle is measured in the document's y-down frame, so "clockwise"
            // would be a claim about the rasteriser that this label is the
            // wrong place to make.
            Action::AngleDown => "Decrease dab angle",
            Action::AngleUp => "Increase dab angle",
            Action::PickupDown => "Decrease colour pickup",
            Action::PickupUp => "Increase colour pickup",
            Action::SwapColours => "Swap colours",
            Action::FitView => "Fit to view",
            Action::ActualSize => "Actual size",
            Action::ZoomIn => "Zoom in",
            Action::ZoomOut => "Zoom out",
        }
    }

    /// Grouping for the settings list.
    pub fn category(self) -> &'static str {
        match self {
            Action::Save | Action::SaveAs | Action::Export => "File",
            Action::Undo
            | Action::Redo
            | Action::Deselect
            | Action::Copy
            | Action::Cut
            | Action::Paste => "Edit",
            // Its own group rather than "Edit": these change the document
            // itself rather than the last thing done to it, and they are the
            // pair every other application files under Image.
            Action::FlipCanvasHorizontal | Action::FlipCanvasVertical => "Image",
            Action::BrushTool
            | Action::EraserTool
            | Action::SelectTool
            | Action::TransformTool
            | Action::PanTool
            | Action::ZoomTool => "Tools",
            // Every temporary brush change, in one run — see `Action::ALL`.
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
            | Action::PickupUp => "Brush",
            Action::SwapColours => "Colour",
            Action::FitView | Action::ActualSize | Action::ZoomIn | Action::ZoomOut => "View",
        }
    }

    /// Stable identifier for the preferences file.
    ///
    /// The debug name is used deliberately: it is already unique, it survives
    /// reordering [`Action::ALL`], and it cannot drift out of step with the
    /// enum the way a hand-written second table would.
    pub fn id(self) -> String {
        format!("{self:?}")
    }

    /// Inverse of [`Action::id`]. Unknown names come from a newer version's
    /// preferences file and are simply dropped.
    pub fn from_id(id: &str) -> Option<Action> {
        Action::ALL.into_iter().find(|a| a.id() == id)
    }
}

/// A key plus the exact modifier state it needs.
///
/// The flags are the *required* state of each modifier, not a mask of
/// modifiers to look for: `ctrl: false` means Ctrl must be up. See [`resolve`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub key: KeyCode,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Chord {
    pub const fn new(key: KeyCode, ctrl: bool, shift: bool, alt: bool) -> Self {
        Self {
            key,
            ctrl,
            shift,
            alt,
        }
    }

    /// Display form for the user, e.g. "Ctrl+Shift+Z", "[", "Space".
    ///
    /// Platform-dependent: the same binding reads "Cmd+Z" on macOS, because
    /// that is what the key is called there. Only the *name* differs — the
    /// stored data is identical everywhere, since [`resolve`] folds Command in
    /// with Ctrl.
    pub fn display(&self) -> String {
        let mut out = String::new();
        // Ctrl, Shift, Alt in that order — the conventional reading order for
        // shortcuts on every desktop platform, so it is worth fixing here
        // rather than following whatever order a binding was written in.
        if self.ctrl {
            out.push_str(primary_modifier_name());
            out.push('+');
        }
        if self.shift {
            out.push_str("Shift+");
        }
        if self.alt {
            out.push_str(alt_modifier_name());
            out.push('+');
        }
        out.push_str(&key_name(self.key));
        out
    }

    /// Form written to the preferences file.
    ///
    /// Deliberately *not* [`Chord::display`]: that spells the primary modifier
    /// "Cmd" on macOS and uses "[" for a key whose name would then have to be
    /// escaped. This form is ASCII-identifier-only and identical on every
    /// platform, so a config file copied between machines still parses.
    pub fn id(&self) -> String {
        let mut out = String::new();
        if self.ctrl {
            out.push_str("Ctrl+");
        }
        if self.shift {
            out.push_str("Shift+");
        }
        if self.alt {
            out.push_str("Alt+");
        }
        out.push_str(&key_id(self.key));
        out
    }

    /// Parse [`Chord::id`]. Anything unrecognised yields `None` rather than a
    /// partial chord, so a corrupt line is dropped whole.
    pub fn from_id(text: &str) -> Option<Chord> {
        let mut chord = Chord::new(KeyCode::KeyA, false, false, false);
        let mut key = None;
        for part in text.split('+') {
            match part {
                "Ctrl" => chord.ctrl = true,
                "Shift" => chord.shift = true,
                "Alt" => chord.alt = true,
                other => key = Some(key_from_id(other)?),
            }
        }
        chord.key = key?;
        Some(chord)
    }
}

/// One key press bound to one action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub action: Action,
    pub key: KeyCode,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Binding {
    pub const fn new(action: Action, chord: Chord) -> Self {
        Self {
            action,
            key: chord.key,
            ctrl: chord.ctrl,
            shift: chord.shift,
            alt: chord.alt,
        }
    }

    pub const fn chord(&self) -> Chord {
        Chord::new(self.key, self.ctrl, self.shift, self.alt)
    }
}

/// The default set, grouped sensibly and ordered by category.
pub fn defaults() -> Vec<Binding> {
    vec![
        // File
        binding(Action::Save, KeyCode::KeyS, true, false, false),
        binding(Action::SaveAs, KeyCode::KeyS, true, true, false),
        // Ctrl+Shift+E, which is what every other painting application binds
        // export to. Plain Ctrl+E is free, but the dialog it opens leads to a
        // file dialog, and the shifted chord is the one people's hands know.
        binding(Action::Export, KeyCode::KeyE, true, true, false),
        // Edit
        binding(Action::Undo, KeyCode::KeyZ, true, false, false),
        binding(Action::Redo, KeyCode::KeyZ, true, true, false),
        // Redo's second binding: Ctrl+Y is what Windows users reach for. The
        // list is a `Vec<Binding>`, not a map keyed by action, precisely so an
        // action can carry more than one.
        binding(Action::Redo, KeyCode::KeyY, true, false, false),
        binding(Action::Deselect, KeyCode::KeyD, true, false, false),
        binding(Action::Copy, KeyCode::KeyC, true, false, false),
        // Ctrl+X, and it does not collide with Swap colours on plain X:
        // `resolve` compares every modifier exactly, which is the same reason
        // Ctrl+Shift+V cannot fire on Paste.
        binding(Action::Cut, KeyCode::KeyX, true, false, false),
        binding(Action::Paste, KeyCode::KeyV, true, false, false),
        // Image
        //
        // H and V for the two axes, which is the only mnemonic anybody
        // remembers, under Ctrl+Shift because the unmodified keys are the pan
        // tool and nothing. Neither chord is taken: `resolve` compares every
        // modifier exactly, so Ctrl+Shift+V cannot fire on Paste's Ctrl+V, and
        // Umber has no paste-special for it to be mistaken for.
        binding(
            Action::FlipCanvasHorizontal,
            KeyCode::KeyH,
            true,
            true,
            false,
        ),
        binding(Action::FlipCanvasVertical, KeyCode::KeyV, true, true, false),
        // Tools
        binding(Action::BrushTool, KeyCode::KeyB, false, false, false),
        binding(Action::EraserTool, KeyCode::KeyE, false, false, false),
        binding(Action::SelectTool, KeyCode::KeyS, false, false, false),
        // T for transform, which is where Photoshop, Krita and Affinity all
        // put it — as Ctrl+T rather than plain T in the first two, but Umber's
        // tool keys are unmodified throughout and a rail that was consistent
        // everywhere except here would be worse than following the crowd.
        binding(Action::TransformTool, KeyCode::KeyT, false, false, false),
        binding(Action::PanTool, KeyCode::KeyH, false, false, false),
        binding(Action::ZoomTool, KeyCode::KeyZ, false, false, false),
        // Brush
        binding(Action::SizeDown, KeyCode::BracketLeft, false, false, false),
        binding(Action::SizeUp, KeyCode::BracketRight, false, false, false),
        // Colour
        binding(Action::SwapColours, KeyCode::KeyX, false, false, false),
        // View
        binding(Action::FitView, KeyCode::Digit0, true, false, false),
        binding(Action::ActualSize, KeyCode::Digit1, true, false, false),
        binding(Action::ZoomIn, KeyCode::Equal, true, false, false),
        // `+` is Shift+= on most layouts, and browsers accept both rather than
        // asking which one the user meant. Modifiers are compared exactly, so
        // the shifted form has to be its own binding — the same reason Redo
        // carries two.
        binding(Action::ZoomIn, KeyCode::Equal, true, true, false),
        binding(Action::ZoomOut, KeyCode::Minus, true, false, false),
    ]
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Find the action bound to this key press in `bindings`, if any.
///
/// Every modifier is compared exactly, including the ones a binding leaves
/// `false`. Treating `false` as "don't care" would make plain Z (zoom tool)
/// fire on Ctrl+Z alongside undo, and would let Ctrl+Z match Redo's
/// Ctrl+Shift+Z. The hand-written `match` this replaces avoided that only
/// through arm ordering, which a user-reorderable table cannot rely on.
pub fn resolve_in(bindings: &[Binding], key: KeyCode, mods: ModifiersState) -> Option<Action> {
    // Command on macOS carries the shortcuts Ctrl carries elsewhere, and winit
    // reports it as Super, so the two are folded together here rather than
    // duplicating every Ctrl binding.
    let chord = Chord::new(
        key,
        mods.control_key() || mods.super_key(),
        mods.shift_key(),
        mods.alt_key(),
    );

    bindings
        .iter()
        .find(|b| b.chord() == chord)
        .map(|b| b.action)
}

/// What the running app dispatches from.
///
/// This is process-global for one reason: the settings page and the key
/// dispatcher never meet. The dispatcher owns a `Vec<Binding>` taken when the
/// window opened; the settings page is handed only the editor state. Publishing
/// the edited table here is what makes a rebind take effect on the very next
/// key press rather than at the next launch.
struct Live {
    bindings: Vec<Binding>,
    /// True while the settings page is listening for a chord. Dispatch is
    /// suspended then, or pressing B to bind it would also switch to the brush.
    capturing: bool,
    /// True while a text field anywhere in the interface has the keyboard.
    ///
    /// Separate from `capturing` rather than folded into it, because the two
    /// have different owners and would otherwise take the lever off one
    /// another: the settings page arms and disarms capture as a field listens,
    /// while typing is a property of the whole frame. Either one suspends
    /// dispatch; neither can clear the other's reason.
    typing: bool,
}

impl Live {
    fn new(bindings: Vec<Binding>) -> Self {
        Self {
            bindings,
            capturing: false,
            typing: false,
        }
    }

    /// Whether a key press should reach the canvas at all.
    fn suspended(&self) -> bool {
        self.capturing || self.typing
    }
}

static LIVE: RwLock<Option<Live>> = RwLock::new(None);

/// Install the table the app should dispatch from.
pub fn publish(bindings: Vec<Binding>) {
    let mut live = write_live();
    match live.as_mut() {
        Some(l) => l.bindings = bindings,
        None => *live = Some(Live::new(bindings)),
    }
}

/// The published table, or the defaults if nothing has been published yet.
pub fn published() -> Vec<Binding> {
    match read_live().as_ref() {
        Some(l) => l.bindings.clone(),
        None => defaults(),
    }
}

/// Suspend or resume dispatch while a chord is being captured.
pub fn set_capturing(capturing: bool) {
    let mut live = write_live();
    match live.as_mut() {
        Some(l) => l.capturing = capturing,
        None => {
            let mut fresh = Live::new(defaults());
            fresh.capturing = capturing;
            *live = Some(fresh);
        }
    }
}

/// Suspend or resume dispatch while a text field has the keyboard.
///
/// Key presses are read straight off the winit event, before egui is asked, so
/// without this every name typed into a search box or a rename field also drives
/// the tool shortcuts — "brush" would select the brush, then the eraser, and a
/// couple more on the way to the end of the word.
///
/// Called from one place, `ui::draw`, for the whole interface rather than per
/// field: a module that pulls the lever for its own fields only ever covers the
/// fields it knows about, and the settings dialog's search box was exactly the
/// one nobody had.
pub fn set_typing(typing: bool) {
    let mut live = write_live();
    match live.as_mut() {
        Some(l) => l.typing = typing,
        None => {
            let mut fresh = Live::new(defaults());
            fresh.typing = typing;
            *live = Some(fresh);
        }
    }
}

/// Find the action bound to this key press.
///
/// `fallback` is the caller's own table, used only until something has been
/// published — which is every unit test, and the handful of frames before
/// preferences are read. Once a table exists it wins, so the caller need not
/// (and cannot) keep its snapshot current.
pub fn resolve(fallback: &[Binding], key: KeyCode, mods: ModifiersState) -> Option<Action> {
    match read_live().as_ref() {
        Some(l) if l.suspended() => None,
        Some(l) => resolve_in(&l.bindings, key, mods),
        None => resolve_in(fallback, key, mods),
    }
}

/// Whether dispatch is suspended: a chord is being captured, or a text field
/// somewhere in the interface has the keyboard.
///
/// The same reading [`resolve`] refuses on, exposed because the three keys
/// [`direct`] covers are handled *before* the table is consulted and would
/// otherwise never ask. With nothing published the answer is `false`, exactly
/// as `resolve` falls through to the caller's own table — a build that has not
/// read its preferences yet has no text field either.
pub fn suspended() -> bool {
    read_live().as_ref().is_some_and(Live::suspended)
}

/// A key that answers a gesture rather than running a command.
///
/// Three keys are decided before the binding table is consulted. Space and
/// Escape are reserved out of [`BINDABLE`] as well — a held modifier has press
/// *and* release meaning, which a press-resolved table cannot express, and a
/// rebindable Escape that sometimes meant nothing would be a row in the
/// settings list that lies. **Enter is the odd one and is genuinely bindable.**
/// Nothing binds it by default, and while a draft or a float is up it is
/// claimed here and a user's own binding does not fire; with neither standing,
/// [`Direct::Finish`] finds nothing to finish and `handle_keys` falls through
/// to [`resolve`], so the binding works. That is the behaviour this replaced
/// and it is left alone deliberately: the alternative is reserving Enter, which
/// would silently drop a chord somebody had already set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direct {
    /// Space: the held pan override.
    PanModifier,
    /// Escape: abandon the gesture in progress.
    Abandon,
    /// Enter: finish the gesture in progress.
    Finish,
}

/// Which of the three untabled keys a press is, if any.
///
/// **They answer to the same suspension the table does, and that was a bug.**
/// Being outside the table used to mean being outside [`resolve`] and therefore
/// outside [`suspended`] as well, so all three reached the canvas while a text
/// field had the keyboard. Every one of them was reachable: Enter in the Text
/// module's caption field inserted a newline *and* committed the floating text
/// into the layer; Escape in a `widgets::number_row` — the typable rail, where
/// Escape means "abandon the figure I typed" — threw away the floating
/// transform behind it, which for a paste is unrecoverable; and a space typed
/// into any field armed the pan override.
///
/// The argument the old comment made for claiming them — that they are claimed
/// only while a draft or a float exists, so they go on reaching whatever else
/// wants them — is about *other* consumers on the canvas. It says nothing about
/// a field that has the keyboard, and a float or a draft is exactly what is
/// standing while somebody types a brush size.
///
/// **A release is never refused, only a press.** Letting go of a held modifier
/// can only ever disarm something, and refusing it is how Space latches: press
/// it over the canvas, click into a field, let go, and the pan override stays
/// armed with no key held and nothing that will clear it. Nothing acts on the
/// release of Escape or Enter, so neither is reported for one.
///
/// **The press side has its own cost and it is accepted rather than
/// overlooked.** egui keeps a field's focus after the pointer leaves it, so
/// `ui::draw`'s `set_typing(ctx.text_edit_focused())` is still true: type in the
/// brush search box, move to the canvas, then hold Space and drag, and you
/// paint where you used to pan. It self-heals, because the press on the canvas
/// takes the focus off the field and the second attempt pans. That is a far
/// better trade than a space typed into a field arming the override — but it is
/// a real behaviour change and the honest fix is for the suspension to mean
/// "the interface has the keyboard" rather than "a `TextEdit` is focused",
/// which is a change to what `set_typing` is told and not to this rule.
pub fn direct(key: KeyCode, pressed: bool, suspended: bool) -> Option<Direct> {
    let direct = match key {
        KeyCode::Space => Direct::PanModifier,
        KeyCode::Escape => Direct::Abandon,
        KeyCode::Enter | KeyCode::NumpadEnter => Direct::Finish,
        _ => return None,
    };
    if !pressed && direct != Direct::PanModifier {
        return None;
    }
    if pressed && suspended {
        return None;
    }
    Some(direct)
}

// A poisoned lock here means a previous panic happened while the table was
// borrowed. The table is a plain `Vec` of `Copy` values, so it cannot be left
// half-updated; taking the inner value keeps shortcuts working rather than
// panicking the paint app a second time.
fn read_live() -> std::sync::RwLockReadGuard<'static, Option<Live>> {
    LIVE.read().unwrap_or_else(|e| e.into_inner())
}

fn write_live() -> std::sync::RwLockWriteGuard<'static, Option<Live>> {
    LIVE.write().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// Editing
// ---------------------------------------------------------------------------

/// Index of the `nth` binding belonging to `action`.
pub fn slot_of(bindings: &[Binding], action: Action, nth: usize) -> Option<usize> {
    bindings
        .iter()
        .enumerate()
        .filter(|(_, b)| b.action == action)
        .map(|(i, _)| i)
        .nth(nth)
}

/// Bindings that already use `chord`, ignoring the one at `except`.
///
/// Returned as indices rather than actions so the caller can both name the
/// clash and remove it.
pub fn users_of(bindings: &[Binding], chord: Chord, except: Option<usize>) -> Vec<usize> {
    bindings
        .iter()
        .enumerate()
        .filter(|(i, b)| Some(*i) != except && b.chord() == chord)
        .map(|(i, _)| i)
        .collect()
}

/// Chords bound to more than one action.
///
/// [`resolve_in`] takes the first match, so a duplicate is not an error the app
/// can notice at run time — it just silently shadows. Duplicates cannot be
/// created through the settings page, but a hand-edited preferences file can
/// carry them, so the page flags them instead of trusting they never happen.
pub fn shadowed(bindings: &[Binding]) -> Vec<Chord> {
    let mut out: Vec<Chord> = Vec::new();
    for (i, b) in bindings.iter().enumerate() {
        let chord = b.chord();
        if out.contains(&chord) {
            continue;
        }
        if bindings[..i].iter().any(|other| other.chord() == chord) {
            out.push(chord);
        }
    }
    out
}

/// Give `chord` to `action`, replacing the binding at `at` or appending a new
/// one, and report anything else that already holds it.
///
/// The other holders are deliberately **left in place**. Quietly taking a chord
/// off another command is the silent drop this whole module exists to prevent —
/// the user would return to a shortcut that had vanished with no record of why.
/// The clash is allowed to exist, named on both rows, and left for the user to
/// settle.
pub fn bind(
    bindings: &mut Vec<Binding>,
    action: Action,
    at: Option<usize>,
    chord: Chord,
) -> Vec<Action> {
    let clashes = users_of(bindings, chord, at)
        .into_iter()
        .map(|i| bindings[i].action)
        .collect();

    match at.and_then(|i| bindings.get_mut(i)) {
        Some(existing) => *existing = Binding::new(action, chord),
        None => bindings.push(Binding::new(action, chord)),
    }
    clashes
}

/// Other commands that hold the same chord as the binding at `index`.
///
/// Named per row rather than summarised once, because "something clashes
/// somewhere" is not actionable and "Undo also uses this" is.
pub fn clashes_with(bindings: &[Binding], index: usize) -> Vec<Action> {
    let Some(binding) = bindings.get(index) else {
        return Vec::new();
    };
    users_of(bindings, binding.chord(), Some(index))
        .into_iter()
        .map(|i| bindings[i].action)
        .filter(|a| *a != binding.action)
        .collect()
}

/// Drop one binding.
pub fn remove(bindings: &mut Vec<Binding>, index: usize) {
    if index < bindings.len() {
        bindings.remove(index);
    }
}

/// Leave `action` with no binding at all.
pub fn clear_action(bindings: &mut Vec<Binding>, action: Action) {
    bindings.retain(|b| b.action != action);
}

/// Restore `action`'s factory bindings, leaving every other action alone.
///
/// A restored chord can collide with something the user bound in the meantime.
/// That collision is left in place rather than resolved silently — [`shadowed`]
/// finds it and the settings page marks it, which is the honest outcome: the
/// user asked for this chord back and is told what it now clashes with.
pub fn reset_action(bindings: &mut Vec<Binding>, action: Action) {
    clear_action(bindings, action);
    bindings.extend(defaults().into_iter().filter(|b| b.action == action));
}

/// True when `action` still holds exactly its factory bindings.
pub fn is_default(bindings: &[Binding], action: Action) -> bool {
    let mine: Vec<Chord> = chords_for(bindings, action);
    let theirs: Vec<Chord> = chords_for(&defaults(), action);
    mine == theirs
}

/// `Brush (B)` — a label with the chord that runs it, as the live table has it.
///
/// The tool rail and the colour wells used to spell their own keys out, which
/// made them a second copy of the binding table: rebind the brush and the
/// tooltip carried on naming `B`. This reads the table that is actually
/// dispatching, so a tooltip cannot say something the keyboard will not do, and
/// an action the user has unbound simply loses its bracket.
pub fn labelled(name: &str, action: Action) -> String {
    match first_chord(action) {
        Some(chord) => format!("{name} ({chord})"),
        None => name.to_owned(),
    }
}

/// The first chord bound to `action` in the live table, ready to show.
pub fn first_chord(action: Action) -> Option<String> {
    let first = |bindings: &[Binding]| {
        bindings
            .iter()
            .find(|b| b.action == action)
            .map(|b| b.chord().display())
    };
    match read_live().as_ref() {
        Some(l) => first(&l.bindings),
        None => first(&defaults()),
    }
}

/// Every chord bound to `action`, in table order.
pub fn chords_for(bindings: &[Binding], action: Action) -> Vec<Chord> {
    bindings
        .iter()
        .filter(|b| b.action == action)
        .map(|b| b.chord())
        .collect()
}

/// Terse constructor, so the default table reads as a table.
const fn binding(action: Action, key: KeyCode, ctrl: bool, shift: bool, alt: bool) -> Binding {
    Binding {
        action,
        key,
        ctrl,
        shift,
        alt,
    }
}

// ---------------------------------------------------------------------------
// Keys
// ---------------------------------------------------------------------------

/// What the primary modifier is called on this platform.
///
/// winit reports Command as Super and [`resolve`] folds the two together, so
/// the stored binding is identical everywhere and only the name differs.
/// Printing "Ctrl" on a Mac would simply be wrong.
pub const fn primary_modifier_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "Cmd"
    } else {
        "Ctrl"
    }
}

/// What the Alt modifier is called on this platform.
pub const fn alt_modifier_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "Option"
    } else {
        "Alt"
    }
}

/// Every key a shortcut may use.
///
/// Deliberately narrower than winit's `KeyCode`:
///
/// - **Modifiers** cannot be bound alone; they are the other half of a chord.
/// - **Numpad keys** are absent because the key events the UI receives collapse
///   the numpad onto the digit row, so Numpad 1 and 1 would be indistinguishable
///   at capture time and the binding would not do what the label said.
/// - **Space** is reserved: it is a held pan modifier with press *and* release
///   meaning, which a press-resolved table cannot express.
/// - **Escape** is reserved: it is what cancels capture.
pub const BINDABLE: &[KeyCode] = &[
    KeyCode::KeyA,
    KeyCode::KeyB,
    KeyCode::KeyC,
    KeyCode::KeyD,
    KeyCode::KeyE,
    KeyCode::KeyF,
    KeyCode::KeyG,
    KeyCode::KeyH,
    KeyCode::KeyI,
    KeyCode::KeyJ,
    KeyCode::KeyK,
    KeyCode::KeyL,
    KeyCode::KeyM,
    KeyCode::KeyN,
    KeyCode::KeyO,
    KeyCode::KeyP,
    KeyCode::KeyQ,
    KeyCode::KeyR,
    KeyCode::KeyS,
    KeyCode::KeyT,
    KeyCode::KeyU,
    KeyCode::KeyV,
    KeyCode::KeyW,
    KeyCode::KeyX,
    KeyCode::KeyY,
    KeyCode::KeyZ,
    KeyCode::Digit0,
    KeyCode::Digit1,
    KeyCode::Digit2,
    KeyCode::Digit3,
    KeyCode::Digit4,
    KeyCode::Digit5,
    KeyCode::Digit6,
    KeyCode::Digit7,
    KeyCode::Digit8,
    KeyCode::Digit9,
    KeyCode::F1,
    KeyCode::F2,
    KeyCode::F3,
    KeyCode::F4,
    KeyCode::F5,
    KeyCode::F6,
    KeyCode::F7,
    KeyCode::F8,
    KeyCode::F9,
    KeyCode::F10,
    KeyCode::F11,
    KeyCode::F12,
    KeyCode::Backquote,
    KeyCode::Minus,
    KeyCode::Equal,
    KeyCode::BracketLeft,
    KeyCode::BracketRight,
    KeyCode::Backslash,
    KeyCode::Semicolon,
    KeyCode::Quote,
    KeyCode::Comma,
    KeyCode::Period,
    KeyCode::Slash,
    KeyCode::Tab,
    KeyCode::Enter,
    KeyCode::Backspace,
    KeyCode::Delete,
    KeyCode::Insert,
    KeyCode::Home,
    KeyCode::End,
    KeyCode::PageUp,
    KeyCode::PageDown,
    KeyCode::ArrowUp,
    KeyCode::ArrowDown,
    KeyCode::ArrowLeft,
    KeyCode::ArrowRight,
];

/// The punctuation keys, with the two legends a US keyboard prints on each —
/// unshifted first.
///
/// Both legends are listed because which one is unshifted is itself
/// layout-dependent: `+` is Shift+= on a US board and unshifted on a Nordic
/// one, and both must reach [`KeyCode::Equal`].
///
/// **One table, read in both directions.** [`key_for_text`] folds a typed
/// character onto the position that prints it, and `keylayout::name_for` asks
/// the other way round — which of a key's legends this keyboard actually has.
/// Two copies of it would be two things to keep in step, and they would drift
/// the first time a key was added to one of them.
pub const PUNCTUATION: [(KeyCode, [char; 2]); 11] = [
    (KeyCode::Backquote, ['`', '~']),
    (KeyCode::Minus, ['-', '_']),
    (KeyCode::Equal, ['=', '+']),
    (KeyCode::BracketLeft, ['[', '{']),
    (KeyCode::BracketRight, [']', '}']),
    (KeyCode::Backslash, ['\\', '|']),
    (KeyCode::Semicolon, [';', ':']),
    (KeyCode::Quote, ['\'', '"']),
    (KeyCode::Comma, [',', '<']),
    (KeyCode::Period, ['.', '>']),
    (KeyCode::Slash, ['/', '?']),
];

/// The bindable key a layout prints `typed` on, for the punctuation keys.
///
/// Bindings are *physical* positions: winit's `KeyCode` names the key where a
/// US layout has that legend, whatever the user's keyboard actually prints on
/// it. For letters that is the right answer — B is in the same place on nearly
/// every Latin layout, and a tool shortcut is a place your hand goes.
///
/// For punctuation it is emphatically not. A Nordic layout gives `+` a key of
/// its own, in the position a US layout prints `-` at, and `-` moves to where
/// US has `/`:
///
/// ```text
/// Nordic:  1 2 3 4 5 6 7 8 9 0 + ´
/// US:      1 2 3 4 5 6 7 8 9 0 - =
/// ```
///
/// So Ctrl and the key marked `+` zoomed *out*, and the key marked `-` did
/// nothing at all. Nobody thinks of Ctrl+= as a position; they think of it as
/// the key with a plus on it. This folds a press onto the US position printing
/// the same character, so a punctuation binding follows the legend on the
/// user's own keyboard.
///
/// Returns `None` for anything not a single bindable punctuation character —
/// letters and digits included, deliberately, so they keep their positions.
pub fn key_for_text(typed: &str) -> Option<KeyCode> {
    let mut chars = typed.chars();
    let first = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    PUNCTUATION
        .iter()
        .find(|(_, legends)| legends.contains(&first))
        .map(|(key, _)| *key)
}

/// The key to dispatch on, given what winit reported and what the layout says
/// that key prints. See [`key_for_text`].
///
/// `typed` is the event's *logical* key where that is a character, and `None`
/// for everything else — a named key, or a dead key such as the `´` a Nordic
/// layout puts where US has `=`.
pub fn typed_key(physical: KeyCode, typed: Option<&str>) -> KeyCode {
    typed.and_then(key_for_text).unwrap_or(physical)
}

/// Stable identifier for the preferences file, e.g. "KeyZ", "BracketLeft".
pub fn key_id(key: KeyCode) -> String {
    format!("{key:?}")
}

/// Inverse of [`key_id`], restricted to [`BINDABLE`].
///
/// Restricting it is what stops a hand-edited file from binding a key the
/// capture field could never produce — a binding the user could then neither
/// trigger nor see the origin of.
pub fn key_from_id(id: &str) -> Option<KeyCode> {
    BINDABLE.iter().copied().find(|k| key_id(*k) == id)
}

/// The printable name of a physical key, as the user's own keyboard has it.
///
/// Bindings are positions and the stored form names positions, but a *label*
/// has to name a key somebody can find. `keylayout` asks the platform what this
/// keyboard prints and answers `None` where it cannot say — on macOS and Linux,
/// for a dead key, and for anything with no legend at all — so the US name
/// below is the floor. A label is never empty and never guesses.
///
/// Only the display varies. [`Chord::id`] is untouched, for the reason its own
/// documentation gives.
pub fn key_name(key: KeyCode) -> String {
    crate::keylayout::key_name(key).unwrap_or_else(|| us_key_name(key))
}

/// The name winit's own `KeyCode` is written for: the legend a *US* keyboard
/// prints at that position.
///
/// What every label said before layouts were asked, and still the fallback
/// whenever the platform will not answer. Deliberately not layout-aware — this
/// is the fixed point [`key_name`] falls back to, so it must not itself depend
/// on the keyboard.
pub fn us_key_name(key: KeyCode) -> String {
    let named = match key {
        KeyCode::Space => "Space",
        KeyCode::BracketLeft => "[",
        KeyCode::BracketRight => "]",
        KeyCode::Minus => "-",
        KeyCode::Equal => "=",
        KeyCode::Comma => ",",
        KeyCode::Period => ".",
        KeyCode::Slash => "/",
        KeyCode::Backslash => "\\",
        KeyCode::Semicolon => ";",
        KeyCode::Quote => "'",
        KeyCode::Backquote => "`",
        KeyCode::Tab => "Tab",
        KeyCode::Enter => "Enter",
        KeyCode::Escape => "Esc",
        KeyCode::Backspace => "Backspace",
        KeyCode::Delete => "Delete",
        KeyCode::Insert => "Insert",
        KeyCode::Home => "Home",
        KeyCode::End => "End",
        KeyCode::PageUp => "Page Up",
        KeyCode::PageDown => "Page Down",
        KeyCode::ArrowUp => "Up",
        KeyCode::ArrowDown => "Down",
        KeyCode::ArrowLeft => "Left",
        KeyCode::ArrowRight => "Right",
        // The letter and digit rows debug-print as "KeyB" and "Digit0", so
        // trimming the prefix beats thirty-six explicit arms. Anything else
        // keeps winit's own name, which is at least recognisable ("F1").
        other => {
            let name = format!("{other:?}");
            return match name
                .strip_prefix("Key")
                .or_else(|| name.strip_prefix("Digit"))
            {
                Some(rest) => rest.to_string(),
                None => name,
            };
        }
    };
    named.to_string()
}

// ---------------------------------------------------------------------------
// A keymap as a file
// ---------------------------------------------------------------------------

/// The extension a keymap carries.
pub const KEYMAP_EXTENSION: &str = "umberkeys";

/// The first line of every keymap Umber writes.
///
/// **[`read_keymap`] does not look at it, and that is deliberate rather than an
/// oversight.** It is there so somebody opening the file in a text editor knows
/// what they are looking at. Requiring it would refuse a list typed by hand and
/// refuse the preferences file itself, both of which are the same lines; and it
/// could not carry a version either, because the format is line-based and every
/// line is independent, so a later revision adding a line kind costs an older
/// reader that one line — which is already counted and reported.
///
/// What decides whether a file is a keymap is therefore whether it names a
/// command Umber knows. Nothing here may start reading this constant back
/// without answering the two refusals above.
const KEYMAP_HEADER: &str = "# Umber keymap";

/// A keymap as a file's worth of text.
///
/// **Ids, never labels.** A shortcut's *label* follows the user's keyboard —
/// `keylayout` asks the platform what a key prints, so the same binding reads
/// "Ctrl++" on a Nordic board and "Ctrl+=" on a US one — and the stored form
/// never may, for [`Chord::id`]'s own reason: the file has to parse on the next
/// machine. So this writes exactly what the preferences file writes, through
/// exactly the same two functions.
///
/// **Every action is written, including the ones at their factory binding and
/// the ones deliberately unbound.** That is the one place this differs from
/// `prefs::to_text`, which writes only what the user changed — and the two want
/// opposite things. There, silence means "this build's default", so a release
/// that adds a command arrives with it bound rather than blank. Here, silence
/// would mean a keymap that quietly took whatever the *receiving* build thought
/// the default was, which for a file whose whole purpose is to carry one
/// keyboard onto another machine is the failure it exists to prevent.
///
/// **The line ending is `\n` on every platform, deliberately.** `docformat`'s
/// rule: a file that travels may not take the platform's ending, or the same
/// keymap written on Windows and on Linux differs byte for byte. That rule was
/// written about a document and a keymap is the same kind of thing; a
/// preferences file, which stays on the machine that wrote it, is not.
pub fn to_keymap(bindings: &[Binding]) -> String {
    let mut out = String::from(KEYMAP_HEADER);
    out.push_str(
        "\n# Written by Umber. One line per command, naming the command and the\n\
         # key in a spelling that is the same on every keyboard and every\n\
         # platform. A command with no key after it is deliberately unbound.\n\n",
    );
    for action in Action::ALL {
        let chords = chords_for(bindings, action);
        if chords.is_empty() {
            out.push_str(&format!("shortcut = {}\n", action.id()));
            continue;
        }
        for chord in chords {
            out.push_str(&format!("shortcut = {} {}\n", action.id(), chord.id()));
        }
    }
    out
}

/// What reading a keymap produced, and everything it could not read.
///
/// The counts are separate rather than one total because each is a different
/// sentence, and **an import that loses something must say so** — the rule every
/// importer in Umber keeps. A file naming a command this build has never heard
/// of is a keymap from a newer Umber; a chord that will not parse is a corrupt
/// or hand-mangled line; a command the file never mentions is a keymap from an
/// older one. Rolled into one number, none of the three could be explained.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Keymap {
    /// The whole table to publish: the file's answers over the current set.
    pub bindings: Vec<Binding>,
    /// Lines naming a command this build does not have.
    pub unknown_actions: usize,
    /// Lines naming a key this build cannot read. The commands they named keep
    /// whatever they were bound to.
    pub unreadable_chords: usize,
    /// Lines that are not blank, not a comment and not a binding at all.
    pub unreadable_lines: usize,
    /// Commands the file never mentions, which keep whatever they were bound
    /// to.
    pub untouched: usize,
}

/// Read a keymap over the bindings in force, or `None` where the text is not a
/// keymap at all.
///
/// `None` is a *refusal* and the caller has to report it: picking the wrong file
/// out of a dialog is the ordinary way to get here, and a silent no-op would
/// look exactly like a keymap that happened to bind nothing.
///
/// Tolerance is `prefs::parse_shortcuts`' and deliberately identical to it:
///
/// - An action the file mentions is **fully described** by the file, including
///   "mentioned with no chord", which is how a deliberate unbinding is said.
/// - **A line whose chord does not parse disqualifies its action from being
///   mentioned at all**, so a stray byte restores a shortcut rather than
///   removing one. Losing a binding to corruption and having no way to tell is
///   the worse failure.
/// - An action the file never mentions keeps what it has. Not what it *defaults*
///   to: an older keymap simply predates a command, and resetting it would undo
///   a choice the file says nothing about.
///
/// A chord the file gives to two commands is kept on both. That is what
/// [`bind`] already does when somebody types a clash into the pane, and the
/// clash is drawn on both rows rather than silently dropped — but the caller has
/// to say it happened, which [`shadowed`] is for.
pub fn read_keymap(text: &str, over: &[Binding]) -> Option<Keymap> {
    let mut out = Keymap::default();
    // Every action the file names at all, broken lines included. What
    // `Keymap::untouched` counts is what is *missing* from this, so an action
    // whose only line would not parse is reported once, as an unreadable chord,
    // rather than twice under two different explanations.
    let mut named: Vec<Action> = Vec::new();
    let mut broken: Vec<Action> = Vec::new();
    let mut custom: Vec<Binding> = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // The `shortcut =` key is optional on the way in and always written on
        // the way out, so a keymap can be pasted straight into a preferences
        // file and a hand-written list of bare pairs still reads.
        let body = match line.split_once('=') {
            Some((key, value)) if key.trim() == "shortcut" => value.trim(),
            Some(_) => {
                out.unreadable_lines += 1;
                continue;
            }
            None => line,
        };
        let mut parts = body.split_whitespace();
        let Some(name) = parts.next() else {
            out.unreadable_lines += 1;
            continue;
        };
        let Some(action) = Action::from_id(name) else {
            out.unknown_actions += 1;
            continue;
        };
        if !named.contains(&action) {
            named.push(action);
        }
        let Some(chord_id) = parts.next() else {
            continue; // Deliberately unbound.
        };
        match Chord::from_id(chord_id) {
            Some(chord) => custom.push(Binding::new(action, chord)),
            None => {
                out.unreadable_chords += 1;
                if !broken.contains(&action) {
                    broken.push(action);
                }
            }
        }
    }
    // **At least one line has to name a command this build knows**, or this is
    // not a keymap. Picking the wrong file out of a dialog is the ordinary way
    // to get here, and every other bar was worse: demanding the header refuses a
    // list somebody typed and refuses the preferences file itself, while
    // accepting any file at all reads a paragraph of prose as thirty unknown
    // commands and reports "imported, with notes".
    if named.is_empty() {
        return None;
    }

    // Ordered by `Action::ALL` whatever order the file used, for
    // `parse_shortcuts`' reason: `resolve_in` gives a chord held twice to
    // whichever binding comes first, so file order would let an unrelated line
    // decide which of two clashing commands runs.
    for action in Action::ALL {
        let described = named.contains(&action) && !broken.contains(&action);
        if described {
            out.bindings
                .extend(custom.iter().copied().filter(|b| b.action == action));
        } else {
            if !named.contains(&action) {
                out.untouched += 1;
            }
            out.bindings
                .extend(over.iter().copied().filter(|b| b.action == action));
        }
    }
    Some(out)
}

#[cfg(test)]
mod keymap_tests {
    use super::*;

    /// A keymap written and read back is the table it was written from.
    ///
    /// Read back over a table that is *not* the one it was written from, so the
    /// assertion measures the file rather than the fallback: over the defaults,
    /// a keymap that carried nothing at all would pass.
    #[test]
    fn a_keymap_written_and_read_back_is_the_table_it_came_from() {
        let mut mine = defaults();
        let at = slot_of(&mine, Action::BrushTool, 0);
        bind(
            &mut mine,
            Action::BrushTool,
            at,
            Chord::new(KeyCode::F7, true, true, false),
        );
        clear_action(&mut mine, Action::ActualSize);
        bind(
            &mut mine,
            Action::Redo,
            None,
            Chord::new(KeyCode::KeyR, true, false, true),
        );

        let text = to_keymap(&mine);
        let read = read_keymap(&text, &defaults()).expect("its own output is a keymap");
        // Per action rather than as one slice, because `bind` appends where the
        // reader orders by `Action::ALL` — which is the *reader's* rule and is
        // asserted separately below rather than folded into this comparison.
        for action in Action::ALL {
            assert_eq!(
                chords_for(&read.bindings, action),
                chords_for(&mine, action),
                "{} came back different",
                action.id(),
            );
        }
        let order: Vec<Action> = read.bindings.iter().map(|b| b.action).collect();
        let expected: Vec<Action> = Action::ALL
            .into_iter()
            .flat_map(|a| std::iter::repeat_n(a, chords_for(&mine, a).len()))
            .collect();
        assert_eq!(
            order, expected,
            "the reader must order by the action list, whatever order the file used",
        );
        assert_eq!(read.untouched, 0, "its own output names every command");
        assert_eq!(read.unknown_actions, 0);
        assert_eq!(read.unreadable_chords, 0);
        assert_eq!(read.unreadable_lines, 0);
    }

    /// Every command is in the file, including the ones nobody has touched and
    /// the ones deliberately unbound.
    ///
    /// This is the one place a keymap differs from the preferences file, and the
    /// difference is load-bearing: silence there means "this build's default",
    /// which is exactly what a file carrying one keyboard onto another machine
    /// must not fall back to.
    #[test]
    fn a_keymap_names_every_command_bound_or_not() {
        let mut mine = defaults();
        clear_action(&mut mine, Action::ZoomTool);
        let text = to_keymap(&mine);
        for action in Action::ALL {
            assert!(
                text.contains(&format!("shortcut = {}", action.id())),
                "{} is not in the file",
                action.id(),
            );
        }
        assert!(text.contains(&format!("shortcut = {}\n", Action::ZoomTool.id())));
    }

    /// The file records ids, never the labels a keyboard prints.
    ///
    /// `keylayout` asks the platform what a key prints, so a label follows the
    /// board in front of the artist; the stored form never may, because the file
    /// has to parse on the next machine. `Chord::id` says so and this is what
    /// holds the keymap to it.
    #[test]
    fn a_keymap_records_ids_and_never_the_keys_a_board_prints() {
        let chord = Chord::new(KeyCode::KeyZ, true, false, false);
        assert_ne!(
            chord.id(),
            chord.display(),
            "pick a chord whose two spellings differ, or this tests nothing",
        );
        let text = to_keymap(&defaults());
        assert!(text.contains(&chord.id()), "the id is not in the file");
        assert!(
            !text.contains(&chord.display()),
            "the file carries a label, which is what the next machine cannot read",
        );
    }

    /// A line Umber cannot read costs that line, and a chord it cannot read
    /// leaves the command it named exactly as it was.
    ///
    /// The second half is `prefs::parse_shortcuts`' rule and the direction
    /// matters: a stray byte must restore a shortcut rather than remove one.
    /// Each loss is counted separately, because each is a different sentence to
    /// put in front of somebody.
    #[test]
    fn a_keymap_line_that_will_not_read_costs_only_itself() {
        let text = concat!(
            "# Umber keymap\n",
            "shortcut = Kaleidoscope Ctrl+KeyK\n",
            "shortcut = Undo Ctrl+KeyGarbage\n",
            "this is not a shortcut = at all\n",
            "shortcut = PanTool KeyP\n",
        );
        let read = read_keymap(text, &defaults()).expect("one line names a real command");
        assert_eq!(read.unknown_actions, 1);
        assert_eq!(read.unreadable_chords, 1);
        assert_eq!(read.unreadable_lines, 1);
        assert_eq!(
            chords_for(&read.bindings, Action::Undo),
            chords_for(&defaults(), Action::Undo),
            "a chord that would not read took a shortcut away",
        );
        assert_eq!(
            chords_for(&read.bindings, Action::PanTool),
            vec![Chord::new(KeyCode::KeyP, false, false, false)],
        );
    }

    /// A command the file never mentions keeps what it had, and is counted.
    ///
    /// Not what it *defaults* to: an older keymap simply predates a command, and
    /// putting it back to the factory binding would undo a choice the file says
    /// nothing about.
    #[test]
    fn a_command_missing_from_a_keymap_keeps_what_it_had() {
        let mut mine = defaults();
        let at = slot_of(&mine, Action::PanTool, 0);
        bind(
            &mut mine,
            Action::PanTool,
            at,
            Chord::new(KeyCode::F9, false, false, false),
        );
        let read = read_keymap("shortcut = Undo Ctrl+KeyU\n", &mine).expect("a keymap");
        assert_eq!(
            chords_for(&read.bindings, Action::PanTool),
            vec![Chord::new(KeyCode::F9, false, false, false)],
        );
        assert_eq!(read.untouched, Action::ALL.len() - 1);
    }

    /// A command named with no key after it is deliberately unbound, which is
    /// the one thing silence could never say.
    #[test]
    fn a_keymap_can_take_a_shortcut_off() {
        let read = read_keymap("shortcut = Undo\n", &defaults()).expect("a keymap");
        assert!(chords_for(&read.bindings, Action::Undo).is_empty());
        assert!(
            !chords_for(&read.bindings, Action::Redo).is_empty(),
            "unbinding one command took another with it",
        );
    }

    /// A key the file gives to two commands is kept on both, and is findable.
    ///
    /// The same answer `bind` gives when somebody types a clash into a row:
    /// flagged on both rows, never silently dropped. The caller has to be able
    /// to *say* it happened, which is what this pins.
    #[test]
    fn a_key_a_keymap_gives_to_two_commands_is_kept_on_both() {
        let read = read_keymap(
            "shortcut = PanTool Ctrl+KeyZ\nshortcut = Undo Ctrl+KeyZ\n",
            &defaults(),
        )
        .expect("a keymap");
        assert_eq!(
            shadowed(&read.bindings),
            vec![Chord::new(KeyCode::KeyZ, true, false, false)],
        );
    }

    /// A file that names no command Umber knows is refused, rather than read as
    /// a keymap that happened to bind nothing.
    ///
    /// Picking the wrong file out of a dialog is the ordinary way to get here,
    /// and a silent no-op looks exactly like an import that worked.
    #[test]
    fn a_file_that_is_not_a_keymap_is_refused() {
        for not_a_keymap in [
            "",
            "# Umber keymap\n",
            "Dear Umber,\n\nplease paint faster.\n",
            "{\"shortcut\": \"Ctrl+Z\"}\n",
        ] {
            assert!(
                read_keymap(not_a_keymap, &defaults()).is_none(),
                "{not_a_keymap:?} was read as a keymap",
            );
        }
    }

    /// A keymap can be pasted into a preferences file and back, because both
    /// sides write the one spelling.
    ///
    /// Not tidiness: it is what says there is a single statement of what a
    /// shortcut line looks like, rather than a second format that can drift.
    #[test]
    fn a_keymap_reads_with_or_without_the_key_in_front_of_it() {
        let with = read_keymap("shortcut = Undo Ctrl+KeyU\n", &defaults()).expect("a keymap");
        let without = read_keymap("Undo Ctrl+KeyU\n", &defaults()).expect("a keymap");
        assert_eq!(with.bindings, without.bindings);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every action ships bound, except the ones that deliberately do not.
    ///
    /// The exception is the temporary brush changes: fourteen keys nobody
    /// asked for would collide with whatever the painter's hands already know,
    /// and choosing well for everybody across six settings is not possible. So
    /// they ship unbound and bindable — which the settings list already draws,
    /// because it walks [`Action::ALL`] rather than [`defaults`].
    ///
    /// The exception is stated as "is it a brush tweak?" rather than as a
    /// second hand-written list, so an action cannot be excused from this
    /// guard by being added to a list. Which tweaks are *nonetheless* bound —
    /// the size pair, which shipped bound — is pinned by
    /// `tweaks::the_shortcuts_are_all_unbound_by_default_except_size`.
    #[test]
    fn every_action_ships_bound_unless_it_is_a_brush_tweak() {
        let bindings = defaults();
        for action in Action::ALL {
            if crate::tweaks::of_action(action).is_some() {
                continue;
            }
            assert!(
                bindings.iter().any(|b| b.action == action),
                "{action:?} has no default binding"
            );
        }
    }
    use std::collections::HashSet;

    const CTRL: ModifiersState = ModifiersState::CONTROL;
    const SHIFT: ModifiersState = ModifiersState::SHIFT;
    const ALT: ModifiersState = ModifiersState::ALT;
    const SUPER: ModifiersState = ModifiersState::SUPER;
    const NONE: ModifiersState = ModifiersState::empty();

    // The pure form, not the wrapper: the wrapper consults a process-global
    // table, and a test that raced with one that published would be a mystery
    // to debug.
    fn hit(key: KeyCode, mods: ModifiersState) -> Option<Action> {
        resolve_in(&defaults(), key, mods)
    }

    #[test]
    fn plain_z_selects_the_zoom_tool_and_is_not_undo() {
        assert_eq!(hit(KeyCode::KeyZ, NONE), Some(Action::ZoomTool));
    }

    #[test]
    fn ctrl_z_is_undo_and_never_the_zoom_tool() {
        // The whole point of exact modifier matching: an unmodified binding
        // must not fire while Ctrl is held.
        assert_eq!(hit(KeyCode::KeyZ, CTRL), Some(Action::Undo));
    }

    #[test]
    fn ctrl_s_saves_and_ctrl_shift_s_asks_where() {
        // Two commands one Shift apart, which only works because every
        // modifier is compared exactly — otherwise Save as… would also save.
        assert_eq!(hit(KeyCode::KeyS, CTRL), Some(Action::Save));
        assert_eq!(hit(KeyCode::KeyS, CTRL | SHIFT), Some(Action::SaveAs));
        // And plain S is the selection tool, not a third spelling of Save —
        // the same pairing plain Z and Ctrl+Z have, and the same reason.
        assert_eq!(hit(KeyCode::KeyS, NONE), Some(Action::SelectTool));
    }

    #[test]
    fn ctrl_shift_z_is_redo_not_undo() {
        assert_eq!(hit(KeyCode::KeyZ, CTRL | SHIFT), Some(Action::Redo));
    }

    #[test]
    fn ctrl_y_is_the_second_redo_binding() {
        assert_eq!(hit(KeyCode::KeyY, CTRL), Some(Action::Redo));
        assert_eq!(hit(KeyCode::KeyY, NONE), None);
    }

    #[test]
    fn super_stands_in_for_ctrl() {
        // macOS: Cmd+Z must undo without a duplicate table of bindings.
        assert_eq!(hit(KeyCode::KeyZ, SUPER), Some(Action::Undo));
        assert_eq!(hit(KeyCode::KeyZ, SUPER | SHIFT), Some(Action::Redo));
        assert_eq!(hit(KeyCode::Digit0, SUPER), Some(Action::FitView));
    }

    #[test]
    fn unwanted_modifiers_block_a_match() {
        // Shift+B is a capital B, not a request to switch tools; Alt opens
        // menus on Windows and must not leak through either.
        assert_eq!(hit(KeyCode::KeyB, NONE), Some(Action::BrushTool));
        assert_eq!(hit(KeyCode::KeyB, SHIFT), None);
        assert_eq!(hit(KeyCode::KeyB, ALT), None);
        assert_eq!(hit(KeyCode::KeyZ, CTRL | ALT), None);
        assert_eq!(hit(KeyCode::KeyZ, CTRL | SHIFT | ALT), None);
    }

    /// The two canvas flips, and the two chords they sit next to.
    ///
    /// Both share a key with something already bound — H is the pan tool and V
    /// is Paste — so this is the case `resolve_in`'s exact modifier comparison
    /// exists for. Getting it wrong in either direction is a command that fires
    /// when nobody asked: a flipped canvas instead of a paste, or a flip on
    /// every press of H.
    #[test]
    fn the_canvas_flips_do_not_take_the_keys_they_sit_beside() {
        assert_eq!(
            hit(KeyCode::KeyH, CTRL | SHIFT),
            Some(Action::FlipCanvasHorizontal)
        );
        assert_eq!(
            hit(KeyCode::KeyV, CTRL | SHIFT),
            Some(Action::FlipCanvasVertical)
        );
        assert_eq!(hit(KeyCode::KeyH, NONE), Some(Action::PanTool));
        assert_eq!(hit(KeyCode::KeyV, CTRL), Some(Action::Paste));
        assert_eq!(hit(KeyCode::KeyV, NONE), None);
        assert_eq!(hit(KeyCode::KeyH, CTRL), None);
    }

    #[test]
    fn tool_and_view_bindings_match_the_app() {
        assert_eq!(hit(KeyCode::KeyE, NONE), Some(Action::EraserTool));
        assert_eq!(hit(KeyCode::KeyH, NONE), Some(Action::PanTool));
        assert_eq!(hit(KeyCode::KeyX, NONE), Some(Action::SwapColours));
        assert_eq!(hit(KeyCode::BracketLeft, NONE), Some(Action::SizeDown));
        assert_eq!(hit(KeyCode::BracketRight, NONE), Some(Action::SizeUp));
        assert_eq!(hit(KeyCode::Digit0, CTRL), Some(Action::FitView));
        assert_eq!(hit(KeyCode::Digit1, CTRL), Some(Action::ActualSize));
    }

    #[test]
    fn ctrl_plus_and_minus_zoom_the_canvas() {
        // These are the keys egui uses to scale its own interface, and they are
        // the canvas's here — interface scale is a slider in Settings. Both
        // spellings of "plus" reach the same action, since `+` is Shift+= on
        // most layouts and modifiers are compared exactly.
        assert_eq!(hit(KeyCode::Equal, CTRL), Some(Action::ZoomIn));
        assert_eq!(hit(KeyCode::Equal, CTRL | SHIFT), Some(Action::ZoomIn));
        assert_eq!(hit(KeyCode::Minus, CTRL), Some(Action::ZoomOut));
        // Unmodified, they are free for anyone to bind.
        assert_eq!(hit(KeyCode::Equal, NONE), None);
        assert_eq!(hit(KeyCode::Minus, NONE), None);
    }

    #[test]
    fn punctuation_follows_the_legend_on_the_users_keyboard() {
        // A Nordic layout puts `+` on the key US layouts print `-` at, and `-`
        // where US has `/`. Dispatching on the physical position made Ctrl and
        // the key marked `+` zoom *out*.
        let nordic_plus = typed_key(KeyCode::Minus, Some("+"));
        let nordic_minus = typed_key(KeyCode::Slash, Some("-"));
        assert_eq!(hit(nordic_plus, CTRL), Some(Action::ZoomIn));
        assert_eq!(hit(nordic_minus, CTRL), Some(Action::ZoomOut));

        // A US board is unchanged: the character it prints is the one its
        // position is named for, so every press folds onto itself.
        assert_eq!(typed_key(KeyCode::Equal, Some("=")), KeyCode::Equal);
        assert_eq!(typed_key(KeyCode::Minus, Some("-")), KeyCode::Minus);
        // Shift+= is `+` there, and must reach the same key rather than a
        // second one.
        assert_eq!(typed_key(KeyCode::Equal, Some("+")), KeyCode::Equal);
        assert_eq!(
            hit(typed_key(KeyCode::Equal, Some("+")), CTRL | SHIFT),
            Some(Action::ZoomIn)
        );
    }

    #[test]
    fn letters_and_digits_keep_their_positions() {
        // Deliberate: a letter shortcut is a place the hand goes, and B is in
        // the same place on nearly every Latin layout. Folding those onto the
        // legend as well would be a larger change than punctuation needs.
        assert_eq!(typed_key(KeyCode::KeyB, Some("b")), KeyCode::KeyB);
        assert_eq!(typed_key(KeyCode::KeyY, Some("z")), KeyCode::KeyY);
        assert_eq!(typed_key(KeyCode::Digit1, Some("&")), KeyCode::Digit1);
        // A named key or a dead key reports no character at all — `´` sits
        // where US has `=` on a Nordic board and must not become one.
        assert_eq!(typed_key(KeyCode::Equal, None), KeyCode::Equal);
        assert_eq!(typed_key(KeyCode::Space, None), KeyCode::Space);
    }

    #[test]
    fn only_single_bindable_punctuation_is_folded() {
        assert_eq!(key_for_text(""), None);
        assert_eq!(key_for_text("++"), None);
        assert_eq!(key_for_text("€"), None);
        // Every character named must land on a key the capture field can also
        // produce, or a fold would reach a binding nobody could ever set.
        for text in [
            "-", "_", "=", "+", "[", "{", "]", "}", "\\", "|", ";", ":", "'", "\"", ",", "<", ".",
            ">", "/", "?", "`", "~",
        ] {
            let key = key_for_text(text).expect("listed above");
            assert!(
                BINDABLE.contains(&key),
                "{text} folds onto an unbindable key"
            );
        }
    }

    #[test]
    fn unbound_keys_resolve_to_nothing() {
        assert_eq!(hit(KeyCode::KeyQ, NONE), None);
        assert_eq!(hit(KeyCode::Digit0, NONE), None);
        // Space is a held pan modifier, deliberately not a table entry.
        assert_eq!(hit(KeyCode::Space, NONE), None);
    }

    #[test]
    fn every_default_binding_names_a_known_action() {
        for b in defaults() {
            assert!(
                Action::ALL.contains(&b.action),
                "{:?} is missing from Action::ALL",
                b.action
            );
        }
    }

    #[test]
    fn no_two_defaults_share_a_key_combination() {
        // A collision would be resolved silently by table order, which is the
        // failure mode this whole module exists to remove.
        let mut seen = HashSet::new();
        for b in defaults() {
            assert!(
                seen.insert(b.chord()),
                "{} is bound twice",
                b.chord().display()
            );
        }
        assert!(shadowed(&defaults()).is_empty());
    }

    #[test]
    fn defaults_are_grouped_by_category() {
        // The settings dialog prints a heading whenever the category changes,
        // so a category must not reappear once another has started.
        let mut finished: Vec<&str> = Vec::new();
        let mut current = "";
        for b in defaults() {
            let category = b.action.category();
            if category != current {
                assert!(
                    !finished.contains(&category),
                    "category {category} is split up"
                );
                if !current.is_empty() {
                    finished.push(current);
                }
                current = category;
            }
        }
    }

    #[test]
    fn display_spells_out_the_combination() {
        // The primary modifier is named for the platform, so the expectation
        // is composed rather than hard-coded — otherwise this test would fail
        // on macOS, where the very same binding reads "Cmd+Z".
        //
        // The *key* half is composed for the same reason one step further on:
        // what a key is called now depends on the keyboard this is running on,
        // and hard-coding "Z" would fail on a German machine and pass on CI.
        // `keylayout`'s tests pin the naming itself, against readings taken
        // from layouts nobody here has. What is under test here is the
        // composition — that the modifiers are spelled and ordered as designed.
        let ctrl = primary_modifier_name();
        let alt = alt_modifier_name();
        let key = key_name;
        let by = |action: Action| -> String {
            defaults()
                .into_iter()
                .find(|b| b.action == action)
                .expect("action has a binding")
                .chord()
                .display()
        };
        assert_eq!(by(Action::Undo), format!("{ctrl}+{}", key(KeyCode::KeyZ)));
        assert_eq!(
            by(Action::Redo),
            format!("{ctrl}+Shift+{}", key(KeyCode::KeyZ))
        );
        assert_eq!(by(Action::SizeDown), key(KeyCode::BracketLeft));
        assert_eq!(by(Action::SizeUp), key(KeyCode::BracketRight));
        assert_eq!(by(Action::BrushTool), key(KeyCode::KeyB));
        // A digit and a named key are the two no layout renames, so these stay
        // written out.
        assert_eq!(by(Action::FitView), format!("{ctrl}+0"));

        let space = binding(Action::PanTool, KeyCode::Space, false, false, false);
        assert_eq!(space.chord().display(), "Space");
        let everything = binding(Action::PanTool, KeyCode::KeyH, true, true, true);
        assert_eq!(
            everything.chord().display(),
            format!("{ctrl}+Shift+{alt}+{}", key(KeyCode::KeyH))
        );
    }

    #[test]
    fn a_label_is_never_empty_and_the_stored_form_never_moves() {
        // The two halves of the rule. A label may follow the keyboard; the id
        // may not, or a preferences file would stop parsing when copied to a
        // machine with a different layout.
        for key in BINDABLE.iter().copied() {
            assert!(!key_name(key).is_empty(), "{key:?} named itself nothing");
            assert_eq!(key_id(key), format!("{key:?}"));
        }
        let chord = Chord::new(KeyCode::Equal, true, false, false);
        assert_eq!(chord.id(), "Ctrl+Equal");
        assert_eq!(Chord::from_id(&chord.id()), Some(chord));
    }

    #[test]
    fn labels_use_british_spelling() {
        assert_eq!(Action::SwapColours.label(), "Swap colours");
        for action in Action::ALL {
            assert!(!action.label().is_empty());
            assert!(!action.category().is_empty());
        }
    }

    // --- identifiers -------------------------------------------------------

    #[test]
    fn chord_ids_round_trip() {
        for key in BINDABLE.iter().copied() {
            for (ctrl, shift, alt) in [
                (false, false, false),
                (true, false, false),
                (false, true, false),
                (false, false, true),
                (true, true, true),
            ] {
                let chord = Chord::new(key, ctrl, shift, alt);
                assert_eq!(Chord::from_id(&chord.id()), Some(chord), "{}", chord.id());
            }
        }
    }

    #[test]
    fn chord_ids_are_platform_independent() {
        // `display` is allowed to say "Cmd"; the stored form never may, or a
        // preferences file would stop parsing when copied to another machine.
        let chord = Chord::new(KeyCode::KeyZ, true, true, true);
        assert_eq!(chord.id(), "Ctrl+Shift+Alt+KeyZ");
    }

    #[test]
    fn nonsense_chord_ids_are_rejected_whole() {
        assert_eq!(Chord::from_id(""), None);
        assert_eq!(Chord::from_id("Ctrl+"), None);
        assert_eq!(Chord::from_id("Ctrl+Shift"), None);
        assert_eq!(Chord::from_id("Hyper+KeyZ"), None);
        assert_eq!(Chord::from_id("KeyZZ"), None);
        // Reserved keys are not bindable, so their ids must not parse either.
        assert_eq!(Chord::from_id("Space"), None);
        assert_eq!(Chord::from_id("Escape"), None);
        assert_eq!(Chord::from_id("Numpad1"), None);
    }

    #[test]
    fn action_ids_round_trip() {
        for action in Action::ALL {
            assert_eq!(Action::from_id(&action.id()), Some(action));
        }
        assert_eq!(Action::from_id("Teleport"), None);
    }

    #[test]
    fn every_default_binding_uses_a_bindable_key() {
        // A default the capture field could not reproduce would be a shortcut
        // the user can lose but never get back.
        for b in defaults() {
            assert!(BINDABLE.contains(&b.key), "{:?} is not bindable", b.key);
        }
    }

    // --- editing -----------------------------------------------------------

    const Z: Chord = Chord::new(KeyCode::KeyZ, true, false, false);
    const Q: Chord = Chord::new(KeyCode::KeyQ, false, false, false);

    #[test]
    fn binding_a_free_chord_clashes_with_nothing() {
        let mut b = defaults();
        let at = slot_of(&b, Action::BrushTool, 0);
        let clashes = bind(&mut b, Action::BrushTool, at, Q);
        assert!(clashes.is_empty());
        assert_eq!(resolve_in(&b, KeyCode::KeyQ, NONE), Some(Action::BrushTool));
        // The old chord is gone, not merely shadowed.
        assert_eq!(resolve_in(&b, KeyCode::KeyB, NONE), None);
    }

    #[test]
    fn binding_a_taken_chord_reports_the_clash_and_keeps_both() {
        // The design is explicit: conflicts are flagged, never silently
        // dropped. Taking Ctrl+Z off Undo here would be the silent drop — the
        // user would find undo gone with nothing to say so.
        let mut b = defaults();
        let at = slot_of(&b, Action::BrushTool, 0);
        let clashes = bind(&mut b, Action::BrushTool, at, Z);
        assert_eq!(clashes, vec![Action::Undo]);
        assert_eq!(chords_for(&b, Action::Undo), vec![Z]);
        assert_eq!(shadowed(&b), vec![Z]);
    }

    #[test]
    fn binding_with_no_slot_appends_a_second_binding() {
        let mut b = defaults();
        bind(&mut b, Action::BrushTool, None, Q);
        assert_eq!(
            chords_for(&b, Action::BrushTool),
            vec![Chord::new(KeyCode::KeyB, false, false, false), Q]
        );
    }

    #[test]
    fn a_row_names_the_commands_it_clashes_with() {
        let mut b = defaults();
        let at = slot_of(&b, Action::BrushTool, 0);
        bind(&mut b, Action::BrushTool, at, Z);
        let undo = slot_of(&b, Action::Undo, 0).expect("undo binding");
        let brush = slot_of(&b, Action::BrushTool, 0).expect("brush binding");
        assert_eq!(clashes_with(&b, undo), vec![Action::BrushTool]);
        assert_eq!(clashes_with(&b, brush), vec![Action::Undo]);
        // An untouched row names nobody.
        let pan = slot_of(&b, Action::PanTool, 0).expect("pan binding");
        assert!(clashes_with(&b, pan).is_empty());
    }

    #[test]
    fn an_actions_own_second_binding_is_not_a_clash() {
        // Two rows for one command are the same command; flagging them against
        // each other would cry wolf.
        let mut b = defaults();
        bind(&mut b, Action::BrushTool, None, Q);
        bind(&mut b, Action::BrushTool, None, Q);
        let first = slot_of(&b, Action::BrushTool, 1).expect("second brush binding");
        assert!(clashes_with(&b, first).is_empty());
    }

    #[test]
    fn users_of_ignores_the_binding_being_edited() {
        let b = defaults();
        let at = slot_of(&b, Action::Undo, 0);
        assert!(users_of(&b, Z, at).is_empty());
        assert_eq!(users_of(&b, Z, None).len(), 1);
    }

    #[test]
    fn clearing_leaves_an_action_unbound_without_touching_others() {
        let mut b = defaults();
        clear_action(&mut b, Action::Redo);
        assert!(chords_for(&b, Action::Redo).is_empty());
        assert_eq!(resolve_in(&b, KeyCode::KeyY, CTRL), None);
        assert_eq!(resolve_in(&b, KeyCode::KeyZ, CTRL), Some(Action::Undo));
    }

    #[test]
    fn resetting_one_action_restores_all_of_its_bindings() {
        let mut b = defaults();
        clear_action(&mut b, Action::Redo);
        assert!(!is_default(&b, Action::Redo));
        reset_action(&mut b, Action::Redo);
        assert!(is_default(&b, Action::Redo));
        // Redo has two defaults; restoring must bring back both.
        assert_eq!(
            resolve_in(&b, KeyCode::KeyZ, CTRL | SHIFT),
            Some(Action::Redo)
        );
        assert_eq!(resolve_in(&b, KeyCode::KeyY, CTRL), Some(Action::Redo));
    }

    #[test]
    fn a_reset_that_reclaims_a_chord_leaves_a_visible_conflict() {
        // The user gave Undo's Ctrl+Z to the brush, then asked for Undo's
        // default back. Both now hold it. Silently dropping either would be a
        // surprise; the clash is left for the settings page to show.
        let mut b = defaults();
        clear_action(&mut b, Action::Undo);
        let at = slot_of(&b, Action::BrushTool, 0);
        bind(&mut b, Action::BrushTool, at, Z);
        reset_action(&mut b, Action::Undo);
        assert_eq!(shadowed(&b), vec![Z]);
    }

    #[test]
    fn shadowed_finds_each_duplicated_chord_once() {
        let mut b = defaults();
        b.push(Binding::new(Action::PanTool, Z));
        b.push(Binding::new(Action::FitView, Z));
        assert_eq!(shadowed(&b), vec![Z]);
    }

    #[test]
    fn is_default_notices_an_extra_binding() {
        let mut b = defaults();
        assert!(is_default(&b, Action::BrushTool));
        bind(&mut b, Action::BrushTool, None, Q);
        assert!(!is_default(&b, Action::BrushTool));
    }

    // --- suspension --------------------------------------------------------

    /// Both levers, exercised through the process-global table.
    ///
    /// Deliberately the *only* test in this file that touches `LIVE`. Every
    /// other one goes through `resolve_in`, so nothing here can race with them;
    /// a second global test could, and the failure would be a mystery. The
    /// untabled keys are checked here rather than in a second global test for
    /// exactly that reason — they read the same lever through `suspended`, so
    /// they belong in the same run of it.
    #[test]
    fn either_lever_suspends_dispatch_and_neither_clears_the_other() {
        publish(defaults());
        assert_eq!(
            resolve(&[], KeyCode::KeyB, NONE),
            Some(Action::BrushTool),
            "nothing is suspended yet"
        );
        assert!(!suspended());
        assert_eq!(
            direct(KeyCode::Escape, true, suspended()),
            Some(Direct::Abandon)
        );

        // A field listening for a chord: pressing B to bind it must not also
        // select the brush.
        set_capturing(true);
        assert_eq!(resolve(&[], KeyCode::KeyB, NONE), None);
        assert!(suspended());

        // A text field takes the keyboard while that field is still listening.
        // Whichever finishes first must not hand dispatch back to the canvas
        // while the other is still going — this is what a single shared flag
        // got wrong.
        set_typing(true);
        set_capturing(false);
        assert_eq!(resolve(&[], KeyCode::KeyB, NONE), None, "still typing");
        assert!(suspended());

        // And the three keys outside the table are refused by the very same
        // reading. This is the whole defect: Enter in the Text module's caption
        // field committed the floating text, and Escape in a typable rail threw
        // away the transform standing behind it.
        for key in [
            KeyCode::Escape,
            KeyCode::Enter,
            KeyCode::NumpadEnter,
            KeyCode::Space,
        ] {
            assert_eq!(
                direct(key, true, suspended()),
                None,
                "{key:?} reached the canvas out of a text field"
            );
        }
        // The release of Space still lands, or the pan override latches on for
        // the rest of the session.
        assert_eq!(
            direct(KeyCode::Space, false, suspended()),
            Some(Direct::PanModifier)
        );

        set_typing(false);
        assert_eq!(resolve(&[], KeyCode::KeyB, NONE), Some(Action::BrushTool));
        assert!(!suspended());
    }

    // --- the keys outside the table ----------------------------------------

    #[test]
    fn the_untabled_keys_are_named_and_nothing_else_is() {
        for (key, expected) in [
            (KeyCode::Space, Some(Direct::PanModifier)),
            (KeyCode::Escape, Some(Direct::Abandon)),
            (KeyCode::Enter, Some(Direct::Finish)),
            (KeyCode::NumpadEnter, Some(Direct::Finish)),
            (KeyCode::KeyB, None),
            (KeyCode::Tab, None),
        ] {
            assert_eq!(direct(key, true, false), expected, "{key:?}");
        }
    }

    #[test]
    fn a_suspended_press_reaches_nothing_and_a_release_still_disarms() {
        // The failure this pair exists for: a space pressed over the canvas and
        // let go of into a field that has taken the keyboard. Gating the
        // release as well would leave `space_down` true with no key held and
        // nothing that will ever clear it — the pan override armed for the rest
        // of the session.
        assert_eq!(direct(KeyCode::Space, true, true), None);
        assert_eq!(
            direct(KeyCode::Space, false, true),
            Some(Direct::PanModifier)
        );
        assert_eq!(
            direct(KeyCode::Space, false, false),
            Some(Direct::PanModifier)
        );

        // Escape and Enter act on the press alone, suspended or not. Reporting
        // them for a release would run the gesture's answer twice.
        for key in [KeyCode::Escape, KeyCode::Enter, KeyCode::NumpadEnter] {
            assert_eq!(direct(key, true, true), None, "{key:?} while typing");
            assert_eq!(direct(key, false, false), None, "{key:?} released");
            assert_eq!(direct(key, false, true), None, "{key:?} released, typing");
        }
    }

    #[test]
    fn the_untabled_keys_are_the_ones_the_table_cannot_hold() {
        // Space and Escape are reserved out of `BINDABLE` — see its docs — so
        // no rebinding can ever reach them and `direct` is the only thing that
        // answers for either. Enter *is* bindable, and nothing binds it by
        // default; if something ever did, this is the test that would say the
        // two now disagree about one key.
        assert!(!BINDABLE.contains(&KeyCode::Space));
        assert!(!BINDABLE.contains(&KeyCode::Escape));
        assert!(
            !defaults().iter().any(|b| b.key == KeyCode::Enter),
            "Enter is claimed by `direct` before the table is consulted"
        );
    }

    #[test]
    fn slot_of_indexes_within_an_action() {
        let b = defaults();
        let first = slot_of(&b, Action::Redo, 0).expect("first redo binding");
        let second = slot_of(&b, Action::Redo, 1).expect("second redo binding");
        assert_eq!(b[first].key, KeyCode::KeyZ);
        assert_eq!(b[second].key, KeyCode::KeyY);
        assert_eq!(slot_of(&b, Action::Redo, 2), None);
    }
}
