//! Preferences that survive a restart.
//!
//! The file lives in the platform configuration directory — `%APPDATA%\Umber\config`,
//! `~/.config/umber`, `~/Library/Application Support/Umber` — and holds only
//! what the settings page can change. Document contents never go near it.
//!
//! Three things happen to a preferences file in practice, and none of them may
//! stop the app starting:
//!
//! - **It does not exist.** The first run of every install. [`load`] returns
//!   [`Prefs::default`], which is deliberately identical to the values the
//!   editor already starts with, so a missing file changes nothing.
//! - **It is older than the code reading it.** A key that is absent keeps its
//!   default; an action with no `shortcut` line keeps its factory bindings. New
//!   settings and new shortcuts therefore arrive switched on rather than blank.
//! - **It is corrupt** — truncated by a crash, or hand-edited into nonsense.
//!   Parsing is line by line and every line is independent, so a bad line costs
//!   that one setting and nothing else.
//!
//! The format is a flat `key = value` text file rather than JSON or TOML. It
//! needs no dependency, it is trivially hand-editable, and — the reason that
//! matters here — an unrecognised or malformed line is skipped instead of
//! failing the whole document, which is exactly the tolerance the three cases
//! above require.

use crate::colorpicker::{PickerMode, WheelShape};
use crate::editor::Editor;
use crate::shortcuts::{self, Action, Binding, Chord};
use crate::theme::{Accent, ThemeKind};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use umber_core::input::PressureSource;

/// Bumped only when a value's *meaning* changes, which has not happened yet.
/// Unknown keys are already ignored, so adding settings needs no bump.
const VERSION: u32 = 1;

const FILE: &str = "preferences.conf";

/// Bounds on the interface scale, matching the slider in the settings page.
/// Below 0.75 the 11 px type is unreadable; above 2.0 the docked panels eat a
/// small screen entirely.
pub const MIN_SCALE: f32 = 0.75;
pub const MAX_SCALE: f32 = 2.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Prefs {
    pub theme: ThemeKind,
    pub accent: Accent,
    /// egui's zoom factor: everything drawn in points scales by this.
    pub interface_scale: f32,
    pub pressure_source: PressureSource,
    pub pressure_max_speed: f32,
    pub pressure_response: f32,
    /// Whether Umber asks GitHub for the release list when it starts.
    pub check_updates: bool,
    /// Whether the user has been shown what that check does.
    ///
    /// Stored rather than derived, because the notice has to be shown exactly
    /// once per installation and the alternative — inferring it from "is this
    /// the first run?" — has no answer on a machine where the preferences file
    /// could not be written.
    pub update_notice_seen: bool,
    /// Whether the hue wheel's triangle turns to follow the hue.
    ///
    /// The one preference here with no control in the settings dialog: it is
    /// set on the Colour panel, beneath the wheel it applies to, which is the
    /// only place it means anything. A preference is not defined by which
    /// dialog changes it — it is a choice about the workspace that should still
    /// be true tomorrow.
    pub wheel_rotates: bool,
    /// Which of the three pickers the Colour panel shows, and — when it is the
    /// wheel — what sits inside the hue ring.
    ///
    /// Set on the panel rather than in the settings dialog, like
    /// [`Prefs::wheel_rotates`], and kept for the same reason: somebody who
    /// works in sliders should not be handed the wheel again every morning.
    pub picker: PickerMode,
    pub wheel_shape: WheelShape,
    /// Whether a saved document carries its undo history.
    pub save_history: bool,
    /// The complete binding table, already merged with the defaults.
    pub shortcuts: Vec<Binding>,
}

impl Default for Prefs {
    fn default() -> Self {
        let pressure = umber_core::input::PressureModel::default();
        Self {
            theme: ThemeKind::Graphite,
            accent: Accent::Umber,
            interface_scale: 1.0,
            pressure_source: pressure.source,
            pressure_max_speed: pressure.max_speed,
            pressure_response: pressure.responsiveness,
            check_updates: true,
            update_notice_seen: false,
            // What the picker has always done, and what the design draws.
            wheel_rotates: true,
            picker: PickerMode::Wheel,
            wheel_shape: WheelShape::Triangle,
            save_history: true,
            shortcuts: shortcuts::defaults(),
        }
    }
}

// ---------------------------------------------------------------------------
// Where the file lives
// ---------------------------------------------------------------------------

/// The preferences file, or `None` on a system with no home directory —
/// which is a real case in containers and on some CI runners.
pub fn config_path() -> Option<PathBuf> {
    // Empty qualifier and organisation give the shortest sensible path on each
    // platform: `%APPDATA%\Umber\config`, `~/.config/umber`, and
    // `~/Library/Application Support/Umber`.
    let dirs = directories::ProjectDirs::from("", "", "Umber")?;
    Some(dirs.config_dir().join(FILE))
}

/// The path as the settings page shows it, or a plain explanation when there
/// is none. Never a silent blank.
pub fn config_path_label() -> String {
    match config_path() {
        Some(path) => path.display().to_string(),
        None => "unavailable — this system has no configuration directory".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Reading and writing
// ---------------------------------------------------------------------------

/// Read the preferences file, falling back to the defaults for anything the
/// file does not supply or supplies badly.
pub fn load() -> Prefs {
    let Some(path) = config_path() else {
        return Prefs::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => from_text(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Prefs::default(),
        Err(e) => {
            log::warn!("could not read {}: {e}", path.display());
            Prefs::default()
        }
    }
}

/// Queue a write. Returns immediately; the file system work happens on a
/// background thread so that no frame ever waits on a disk.
pub fn save(prefs: &Prefs) {
    let text = to_text(prefs);
    match writer() {
        Some(tx) => {
            if tx.send(text.clone()).is_err() {
                // The writer thread died. Better a blocking write than lost
                // settings — this is a settings interaction, not a stroke.
                write_now(&text);
            }
        }
        None => write_now(&text),
    }
}

/// The background writer, or `None` if the thread could not be started.
fn writer() -> Option<&'static mpsc::Sender<String>> {
    static WRITER: OnceLock<Option<mpsc::Sender<String>>> = OnceLock::new();
    WRITER
        .get_or_init(|| {
            let (tx, rx) = mpsc::channel::<String>();
            let spawned = std::thread::Builder::new()
                .name("umber-prefs".to_owned())
                .spawn(move || {
                    while let Ok(mut text) = rx.recv() {
                        // Coalesce: if several saves queued up while the last
                        // write was in flight, only the newest is worth doing.
                        while let Ok(newer) = rx.try_recv() {
                            text = newer;
                        }
                        write_now(&text);
                    }
                });
            match spawned {
                Ok(_) => Some(tx),
                Err(e) => {
                    log::warn!("could not start the preferences writer: {e}");
                    None
                }
            }
        })
        .as_ref()
}

/// Write via a temporary file and a rename.
///
/// Writing in place would leave a half-written file if the process died
/// mid-write, and a half-written file loses every setting rather than the one
/// being changed. The rename is the closest thing to atomic that std offers.
fn write_now(text: &str) {
    let Some(path) = config_path() else {
        return;
    };
    if let Some(dir) = path.parent()
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        log::warn!("could not create {}: {e}", dir.display());
        return;
    }

    let temp = path.with_extension("tmp");
    if let Err(e) = std::fs::write(&temp, text) {
        log::warn!("could not write {}: {e}", temp.display());
        return;
    }
    // Windows refuses to rename onto an existing file, so the old one goes
    // first. The gap is a few microseconds and a missing file reads as "no
    // preferences yet", which is recoverable; a torn file is not.
    let _ = std::fs::remove_file(&path);
    if let Err(e) = std::fs::rename(&temp, &path) {
        log::warn!("could not replace {}: {e}", path.display());
    }
}

// ---------------------------------------------------------------------------
// The file format
// ---------------------------------------------------------------------------

pub fn to_text(prefs: &Prefs) -> String {
    let mut out = String::new();
    out.push_str("# Umber preferences.\n");
    out.push_str("# Rewritten whenever a setting changes. Lines that do not parse are ignored,\n");
    out.push_str("# so an unknown or malformed setting costs only itself.\n");
    out.push_str(&format!("version = {VERSION}\n"));
    out.push_str(&format!("theme = {}\n", theme_id(prefs.theme)));
    out.push_str(&format!("accent = {}\n", accent_id(prefs.accent)));
    out.push_str(&format!("interface_scale = {:.3}\n", prefs.interface_scale));
    out.push_str(&format!(
        "pressure_source = {}\n",
        pressure_id(prefs.pressure_source)
    ));
    out.push_str(&format!(
        "pressure_max_speed = {:.1}\n",
        prefs.pressure_max_speed
    ));
    out.push_str(&format!(
        "pressure_response = {:.3}\n",
        prefs.pressure_response
    ));
    out.push_str(&format!("check_updates = {}\n", prefs.check_updates));
    out.push_str(&format!(
        "update_notice_seen = {}\n",
        prefs.update_notice_seen
    ));
    out.push_str(&format!("wheel_rotates = {}\n", prefs.wheel_rotates));
    out.push_str(&format!("picker = {}\n", picker_id(prefs.picker)));
    out.push_str(&format!(
        "wheel_shape = {}\n",
        wheel_shape_id(prefs.wheel_shape)
    ));
    out.push_str(&format!("save_history = {}\n", prefs.save_history));

    // Only actions that differ from the factory table are written. An action
    // left out keeps its defaults, which is what lets a later version add a
    // shortcut and have it arrive bound rather than blank.
    out.push('\n');
    for action in Action::ALL {
        if shortcuts::is_default(&prefs.shortcuts, action) {
            continue;
        }
        let chords = shortcuts::chords_for(&prefs.shortcuts, action);
        if chords.is_empty() {
            // An action with no chord is how "the user cleared this" is said —
            // distinct from "the file predates this action", which is silence.
            out.push_str(&format!("shortcut = {}\n", action.id()));
            continue;
        }
        for chord in chords {
            out.push_str(&format!("shortcut = {} {}\n", action.id(), chord.id()));
        }
    }
    out
}

pub fn from_text(text: &str) -> Prefs {
    let mut prefs = Prefs::default();
    let mut shortcut_lines: Vec<&str> = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());

        // Every arm leaves the default in place when the value does not parse,
        // so one bad line never spreads.
        match key {
            "version" => {}
            "accent" => {
                if let Some(a) = accent_from_id(value) {
                    prefs.accent = a;
                }
            }
            "theme" => {
                if let Some(t) = theme_from_id(value) {
                    prefs.theme = t;
                }
            }
            "interface_scale" => {
                if let Some(v) = parse_f32(value, MIN_SCALE, MAX_SCALE) {
                    prefs.interface_scale = v;
                }
            }
            "pressure_source" => {
                if let Some(s) = pressure_from_id(value) {
                    prefs.pressure_source = s;
                }
            }
            "pressure_max_speed" => {
                if let Some(v) = parse_f32(value, 100.0, 20_000.0) {
                    prefs.pressure_max_speed = v;
                }
            }
            "pressure_response" => {
                if let Some(v) = parse_f32(value, 0.01, 1.0) {
                    prefs.pressure_response = v;
                }
            }
            "check_updates" => {
                if let Some(v) = parse_bool(value) {
                    prefs.check_updates = v;
                }
            }
            "update_notice_seen" => {
                if let Some(v) = parse_bool(value) {
                    prefs.update_notice_seen = v;
                }
            }
            "wheel_rotates" => {
                if let Some(v) = parse_bool(value) {
                    prefs.wheel_rotates = v;
                }
            }
            "picker" => {
                if let Some(v) = picker_from_id(value) {
                    prefs.picker = v;
                }
            }
            "wheel_shape" => {
                if let Some(v) = wheel_shape_from_id(value) {
                    prefs.wheel_shape = v;
                }
            }
            "save_history" => {
                if let Some(v) = parse_bool(value) {
                    prefs.save_history = v;
                }
            }
            "shortcut" => shortcut_lines.push(value),
            // A key from a newer version. Ignoring it is what makes the file
            // safe to share between versions of Umber.
            _ => {}
        }
    }

    prefs.shortcuts = parse_shortcuts(&shortcut_lines);
    prefs
}

/// Merge the file's `shortcut` lines over the factory table.
///
/// An action the file mentions is fully described by the file — including
/// "mentioned with no chord", which means the user deliberately unbound it. An
/// action the file never mentions keeps its defaults.
///
/// A line whose chord does not parse disqualifies its action from being treated
/// as mentioned at all, so corruption restores a shortcut rather than removing
/// one. Losing a binding to a stray byte and having no way to tell would be the
/// worse failure.
///
/// The result is ordered by [`Action::ALL`] whatever order the file used. That
/// matters because `resolve_in` gives a chord held twice to whichever binding
/// comes first: with file order, editing an unrelated line could change which
/// of two clashing commands runs. Ordering by the action list makes the winner
/// the one the settings page lists first, which is also the one its warning
/// names.
fn parse_shortcuts(lines: &[&str]) -> Vec<Binding> {
    let mut mentioned: Vec<Action> = Vec::new();
    let mut broken: Vec<Action> = Vec::new();
    let mut custom: Vec<Binding> = Vec::new();

    for line in lines {
        let mut parts = line.split_whitespace();
        let Some(action) = parts.next().and_then(Action::from_id) else {
            continue;
        };
        if !mentioned.contains(&action) {
            mentioned.push(action);
        }
        let Some(chord_id) = parts.next() else {
            continue; // Deliberately unbound.
        };
        match Chord::from_id(chord_id) {
            Some(chord) => custom.push(Binding::new(action, chord)),
            None => {
                if !broken.contains(&action) {
                    broken.push(action);
                }
            }
        }
    }
    mentioned.retain(|a| !broken.contains(a));

    let defaults = shortcuts::defaults();
    let mut out = Vec::with_capacity(defaults.len());
    for action in Action::ALL {
        let source = if mentioned.contains(&action) {
            &custom
        } else {
            &defaults
        };
        out.extend(source.iter().copied().filter(|b| b.action == action));
    }
    out
}

/// Parse a number and clamp it into range.
///
/// Clamping rather than rejecting is deliberate: a hand-edited scale of 40
/// should give the largest interface the app offers, not silently the default.
/// Values that are not numbers at all — or are NaN — are rejected.
fn parse_f32(value: &str, lo: f32, hi: f32) -> Option<f32> {
    let v: f32 = value.parse().ok()?;
    if v.is_nan() {
        None
    } else {
        Some(v.clamp(lo, hi))
    }
}

/// Parse a flag.
///
/// Anything that is not one of the two words leaves the setting at its default,
/// which for the update check means a hand-edited `check_updates = maybe`
/// arrives as "on and announced" rather than as "off and silent".
fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// Stable names for the themes.
///
/// A `match` rather than a derive: it is the point at which someone adding a
/// third theme is forced to choose the name it will be stored under, instead of
/// discovering later that renaming the variant silently reset everyone's theme.
fn theme_id(kind: ThemeKind) -> &'static str {
    match kind {
        ThemeKind::Graphite => "graphite",
        ThemeKind::Paper => "paper",
    }
}

/// Stable names for the accents, for the same reason as `theme_id`.
fn accent_id(accent: Accent) -> &'static str {
    match accent {
        Accent::Umber => "umber",
        Accent::Sage => "sage",
        Accent::Steel => "steel",
        Accent::Clay => "clay",
    }
}

fn accent_from_id(id: &str) -> Option<Accent> {
    Accent::ALL.into_iter().find(|a| accent_id(*a) == id)
}

fn theme_from_id(id: &str) -> Option<ThemeKind> {
    ThemeKind::ALL.into_iter().find(|k| theme_id(*k) == id)
}

/// Stable names for the picker modes, for the same reason as `theme_id`.
///
/// Deliberately not `PickerMode::label` lower-cased: the label is what the
/// interface shows and is free to be reworded, while this is what a file
/// written a year ago still says.
fn picker_id(mode: PickerMode) -> &'static str {
    match mode {
        PickerMode::Wheel => "wheel",
        PickerMode::Square => "square",
        PickerMode::Sliders => "sliders",
    }
}

fn picker_from_id(id: &str) -> Option<PickerMode> {
    PickerMode::ALL.into_iter().find(|m| picker_id(*m) == id)
}

fn wheel_shape_id(shape: WheelShape) -> &'static str {
    match shape {
        WheelShape::Triangle => "triangle",
        WheelShape::Square => "square",
    }
}

fn wheel_shape_from_id(id: &str) -> Option<WheelShape> {
    WheelShape::ALL
        .into_iter()
        .find(|s| wheel_shape_id(*s) == id)
}

fn pressure_id(source: PressureSource) -> &'static str {
    match source {
        PressureSource::Device => "device",
        PressureSource::Simulated => "simulated",
        PressureSource::Constant => "constant",
    }
}

fn pressure_from_id(id: &str) -> Option<PressureSource> {
    [
        PressureSource::Device,
        PressureSource::Simulated,
        PressureSource::Constant,
    ]
    .into_iter()
    .find(|s| pressure_id(*s) == id)
}

// ---------------------------------------------------------------------------
// Applying and capturing
// ---------------------------------------------------------------------------

/// Everything the settings page can change, read back out of the live app.
///
/// Reading the values from where they are actually used, rather than keeping a
/// second copy in a `Prefs` struct alongside them, is what stops the file and
/// the running app drifting apart.
pub fn capture(ctx: &egui::Context, ed: &Editor) -> Prefs {
    Prefs {
        theme: ed.ui.theme,
        accent: ed.ui.accent,
        interface_scale: ctx.zoom_factor(),
        pressure_source: ed.pressure.source,
        pressure_max_speed: ed.pressure.max_speed,
        pressure_response: ed.pressure.responsiveness,
        check_updates: ed.updates.check_on_startup,
        update_notice_seen: ed.updates.notice_seen,
        wheel_rotates: ed.ui.wheel_rotates,
        picker: ed.ui.picker,
        wheel_shape: ed.ui.wheel_shape,
        save_history: ed.ui.save_history,
        shortcuts: shortcuts::published(),
    }
}

/// Push stored preferences into the running app.
pub fn apply(prefs: &Prefs, ctx: &egui::Context, ed: &mut Editor) {
    ed.ui.theme = prefs.theme;
    ed.ui.accent = prefs.accent;
    ed.pressure.source = prefs.pressure_source;
    ed.pressure.max_speed = prefs.pressure_max_speed;
    ed.pressure.responsiveness = prefs.pressure_response;
    ed.updates.check_on_startup = prefs.check_updates;
    ed.updates.notice_seen = prefs.update_notice_seen;
    ed.ui.wheel_rotates = prefs.wheel_rotates;
    ed.ui.picker = prefs.picker;
    ed.ui.wheel_shape = prefs.wheel_shape;
    ed.ui.save_history = prefs.save_history;
    shortcuts::publish(prefs.shortcuts.clone());

    // Setting the zoom factor when it has not changed still marks egui's fonts
    // dirty, which is a full glyph atlas rebuild — so only do it on a change.
    if (ctx.zoom_factor() - prefs.interface_scale).abs() > 1e-4 {
        ctx.set_zoom_factor(prefs.interface_scale.clamp(MIN_SCALE, MAX_SCALE));
    }
}

/// Read and apply the preferences file, once per run.
///
/// This is driven from the settings page rather than from window start-up
/// because preferences are the settings page's own data, and its draw function
/// runs every frame whether the dialog is open or not — so its first call *is*
/// start-up, and nothing else has to know that preferences exist.
///
/// Returns true on the frame it loaded, so the caller can force one more frame:
/// the theme is pushed into egui's style before the UI runs, so a theme read
/// here lands one frame late, and with `ControlFlow::Wait` that frame might
/// otherwise not arrive until the user moved the mouse.
pub fn ensure_loaded(ctx: &egui::Context, ed: &mut Editor) -> bool {
    static LOADED: AtomicBool = AtomicBool::new(false);
    if LOADED.swap(true, Ordering::Relaxed) {
        return false;
    }
    let prefs = load();
    apply(&prefs, ctx, ed);
    true
}

static DIRTY: AtomicBool = AtomicBool::new(false);

/// Note that something the settings page owns has changed.
pub fn mark_dirty() {
    DIRTY.store(true, Ordering::Relaxed);
}

/// Write pending changes once the user has stopped interacting.
///
/// Waiting for the pointer to come up is what keeps a slider drag to one write
/// instead of one per frame, and it means the write never lands in the middle
/// of a gesture. Nothing here can run during a stroke: the settings page is
/// modal, so the canvas is not being drawn on.
pub fn flush_if_idle(ctx: &egui::Context, ed: &Editor) {
    if !DIRTY.load(Ordering::Relaxed) || ctx.input(|i| i.pointer.any_down()) {
        return;
    }
    DIRTY.store(false, Ordering::Relaxed);
    save(&capture(ctx, ed));
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::KeyCode;

    #[test]
    fn defaults_match_what_the_app_starts_with() {
        // A missing preferences file must change nothing, so these two have to
        // agree — otherwise the first run and the second differ.
        let prefs = Prefs::default();
        let editor = Editor::default();
        assert_eq!(prefs.theme, editor.ui.theme);
        assert_eq!(prefs.pressure_source, editor.pressure.source);
        assert_eq!(prefs.pressure_max_speed, editor.pressure.max_speed);
        assert_eq!(prefs.pressure_response, editor.pressure.responsiveness);
        assert_eq!(prefs.check_updates, editor.updates.check_on_startup);
        assert_eq!(prefs.update_notice_seen, editor.updates.notice_seen);
        assert_eq!(prefs.wheel_rotates, editor.ui.wheel_rotates);
        assert_eq!(prefs.picker, editor.ui.picker);
        assert_eq!(prefs.wheel_shape, editor.ui.wheel_shape);
        assert_eq!(prefs.save_history, editor.ui.save_history);
        assert_eq!(prefs.shortcuts, shortcuts::defaults());
    }

    /// The one setting whose control is not in the settings dialog. It is set
    /// on the Colour panel, under the wheel it applies to, and it has to be
    /// carried through the file and back into the editor exactly like the ones
    /// that are — which is the step that would be easy to leave out.
    #[test]
    fn the_wheels_rotation_survives_a_restart() {
        let prefs = Prefs {
            wheel_rotates: false,
            ..Prefs::default()
        };
        let back = from_text(&to_text(&prefs));
        assert!(!back.wheel_rotates, "the file must not lose the choice");

        let ctx = egui::Context::default();
        let mut editor = Editor::default();
        assert!(editor.ui.wheel_rotates, "the default is on");
        apply(&back, &ctx, &mut editor);
        assert!(
            !editor.ui.wheel_rotates,
            "reading the file must reach the picker, not just the Prefs struct"
        );
        assert!(!capture(&ctx, &editor).wheel_rotates, "and back out again");
    }

    /// The picker's own names, which the file is written in, must not be the
    /// interface's — a label is free to be reworded and a stored id is not.
    #[test]
    fn the_pickers_choice_of_picker_survives_a_restart() {
        for mode in PickerMode::ALL {
            for shape in WheelShape::ALL {
                let prefs = Prefs {
                    picker: mode,
                    wheel_shape: shape,
                    ..Prefs::default()
                };
                let back = from_text(&to_text(&prefs));
                assert_eq!(back.picker, mode);
                assert_eq!(back.wheel_shape, shape);
            }
        }
        // Square is a mode *and* a wheel centre, and they are separate keys —
        // one must never be read as the other.
        let prefs = from_text("picker = triangle\nwheel_shape = sliders\n");
        assert_eq!(
            prefs.picker,
            PickerMode::Wheel,
            "a bad id keeps the default"
        );
        assert_eq!(prefs.wheel_shape, WheelShape::Triangle);
    }

    #[test]
    fn the_update_check_defaults_to_on_and_unannounced() {
        // The pair that makes "on by default" defensible: the check is wanted,
        // and nobody has been told about it yet — so the first run shows the
        // notice and holds the request until it is answered. Flipping either of
        // these silently is how an application starts phoning home unannounced.
        let prefs = Prefs::default();
        assert!(prefs.check_updates);
        assert!(!prefs.update_notice_seen);
    }

    #[test]
    fn turning_the_update_check_off_survives_a_restart() {
        let prefs = Prefs {
            check_updates: false,
            update_notice_seen: true,
            ..Prefs::default()
        };
        let back = from_text(&to_text(&prefs));
        assert!(!back.check_updates, "the file must not lose a refusal");
        assert!(back.update_notice_seen, "and must not ask again");
    }

    #[test]
    fn a_corrupt_update_flag_leaves_the_check_announced_rather_than_silent() {
        // The failure direction matters: a setting that could not be read must
        // not become "check, and say nothing about it".
        let prefs = from_text("check_updates = maybe\nupdate_notice_seen = yes\n");
        assert!(prefs.check_updates);
        assert!(!prefs.update_notice_seen);
    }

    /// Saving the history makes documents materially larger, so a refusal has
    /// to survive a restart — and a line that cannot be read has to leave the
    /// setting on, which is the direction that costs disk rather than work.
    #[test]
    fn turning_the_saved_history_off_survives_a_restart() {
        let prefs = Prefs {
            save_history: false,
            ..Prefs::default()
        };
        assert!(!from_text(&to_text(&prefs)).save_history);
        assert!(
            from_text(
                "save_history = sometimes
"
            )
            .save_history
        );
    }

    #[test]
    fn a_full_round_trip_preserves_everything() {
        let mut prefs = Prefs {
            theme: ThemeKind::Paper,
            accent: Accent::Clay,
            interface_scale: 1.25,
            pressure_source: PressureSource::Simulated,
            pressure_max_speed: 1800.0,
            pressure_response: 0.6,
            check_updates: false,
            update_notice_seen: true,
            wheel_rotates: false,
            picker: PickerMode::Sliders,
            wheel_shape: WheelShape::Square,
            save_history: false,
            shortcuts: shortcuts::defaults(),
        };
        let at = shortcuts::slot_of(&prefs.shortcuts, Action::BrushTool, 0);
        shortcuts::bind(
            &mut prefs.shortcuts,
            Action::BrushTool,
            at,
            Chord::new(KeyCode::F5, true, false, true),
        );
        shortcuts::clear_action(&mut prefs.shortcuts, Action::ActualSize);

        let back = from_text(&to_text(&prefs));
        assert_eq!(back.theme, prefs.theme);
        assert_eq!(back.accent, prefs.accent);
        assert_eq!(back.interface_scale, prefs.interface_scale);
        assert_eq!(back.pressure_source, prefs.pressure_source);
        assert_eq!(back.pressure_max_speed, prefs.pressure_max_speed);
        assert_eq!(back.pressure_response, prefs.pressure_response);
        assert_eq!(back.check_updates, prefs.check_updates);
        assert_eq!(back.update_notice_seen, prefs.update_notice_seen);
        assert_eq!(back.wheel_rotates, prefs.wheel_rotates);
        assert_eq!(back.picker, prefs.picker);
        assert_eq!(back.wheel_shape, prefs.wheel_shape);
        assert_eq!(back.save_history, prefs.save_history);
        // Compared per action rather than as one list: editing a binding
        // appends it, so the live table is in interaction order while a loaded
        // one is in `Action::ALL` order. What has to survive is which chords
        // each command holds, and in what order it holds them.
        for action in Action::ALL {
            assert_eq!(
                shortcuts::chords_for(&back.shortcuts, action),
                shortcuts::chords_for(&prefs.shortcuts, action),
                "{action:?}"
            );
        }
    }

    #[test]
    fn loading_orders_bindings_by_the_action_list() {
        // Deterministic whatever order the file is in — the settings page lists
        // actions in this order, and a chord held twice goes to the first.
        let prefs = from_text(concat!(
            "shortcut = ActualSize Ctrl+Digit2\n",
            "shortcut = Undo Ctrl+KeyU\n",
        ));
        let order: Vec<Action> = prefs.shortcuts.iter().map(|b| b.action).collect();
        let expected: Vec<Action> = Action::ALL
            .into_iter()
            .flat_map(|a| std::iter::repeat_n(a, shortcuts::chords_for(&prefs.shortcuts, a).len()))
            .collect();
        assert_eq!(order, expected);
    }

    #[test]
    fn an_empty_file_is_the_defaults() {
        assert_eq!(from_text(""), Prefs::default());
    }

    #[test]
    fn an_older_file_keeps_defaults_for_what_it_does_not_mention() {
        // A file written before interface scale, pressure and shortcuts
        // existed. Everything absent has to arrive at its default rather than
        // at zero.
        let prefs = from_text("version = 1\ntheme = paper\n");
        assert_eq!(prefs.theme, ThemeKind::Paper);
        assert_eq!(prefs.interface_scale, 1.0);
        assert_eq!(prefs.pressure_source, Prefs::default().pressure_source);
        assert_eq!(prefs.shortcuts, shortcuts::defaults());
    }

    #[test]
    fn a_newer_file_ignores_settings_this_version_lacks() {
        let prefs = from_text("theme = paper\nonion_skin_frames = 4\ntheme_gradient = wild\n");
        assert_eq!(prefs.theme, ThemeKind::Paper);
        assert_eq!(
            prefs,
            Prefs {
                theme: ThemeKind::Paper,
                ..Prefs::default()
            }
        );
    }

    #[test]
    fn corrupt_lines_cost_only_themselves() {
        let prefs = from_text(concat!(
            "theme = paper\n",
            "\u{0}\u{0}\u{0} binary rubbish from a truncated write\n",
            "theme = no such theme\n",
            "interface_scale = \n",
            "pressure_response = NaN\n",
            "= = =\n",
            "shortcut\n",
        ));
        assert_eq!(prefs.theme, ThemeKind::Paper, "the good line still applies");
        assert_eq!(prefs.interface_scale, 1.0);
        assert_eq!(prefs.pressure_response, Prefs::default().pressure_response);
        assert_eq!(prefs.shortcuts, shortcuts::defaults());
    }

    #[test]
    fn a_file_of_nothing_but_rubbish_still_starts_the_app() {
        let junk: String = (0..500u32).map(|i| ((i % 250) as u8 + 1) as char).collect();
        assert_eq!(from_text(&junk), Prefs::default());
    }

    #[test]
    fn out_of_range_numbers_are_clamped_not_dropped() {
        let prefs = from_text("interface_scale = 40\n");
        assert_eq!(prefs.interface_scale, MAX_SCALE);
        let prefs = from_text("interface_scale = -3\n");
        assert_eq!(prefs.interface_scale, MIN_SCALE);
    }

    #[test]
    fn an_unbound_action_survives_the_round_trip() {
        // The distinction the format has to carry: "the user cleared this" is
        // not the same as "this file is older than the action".
        let mut prefs = Prefs::default();
        shortcuts::clear_action(&mut prefs.shortcuts, Action::ZoomTool);
        let text = to_text(&prefs);
        assert!(text.contains("shortcut = ZoomTool\n"));
        let back = from_text(&text);
        assert!(shortcuts::chords_for(&back.shortcuts, Action::ZoomTool).is_empty());
        // Everything else is untouched.
        assert!(shortcuts::is_default(&back.shortcuts, Action::BrushTool));
    }

    #[test]
    fn both_of_an_actions_bindings_round_trip() {
        let mut prefs = Prefs::default();
        shortcuts::bind(
            &mut prefs.shortcuts,
            Action::Redo,
            None,
            Chord::new(KeyCode::KeyR, true, false, false),
        );
        let back = from_text(&to_text(&prefs));
        assert_eq!(
            shortcuts::chords_for(&back.shortcuts, Action::Redo),
            shortcuts::chords_for(&prefs.shortcuts, Action::Redo)
        );
    }

    #[test]
    fn only_customised_shortcuts_are_written() {
        // Writing the whole table would freeze today's defaults into every
        // user's file, so a later change to a default would never reach anyone.
        let mut prefs = Prefs::default();
        assert!(!to_text(&prefs).contains("shortcut ="));

        let at = shortcuts::slot_of(&prefs.shortcuts, Action::PanTool, 0);
        shortcuts::bind(
            &mut prefs.shortcuts,
            Action::PanTool,
            at,
            Chord::new(KeyCode::KeyP, false, false, false),
        );
        let text = to_text(&prefs);
        assert!(text.contains("shortcut = PanTool KeyP\n"));
        assert!(!text.contains("BrushTool"));
    }

    #[test]
    fn a_shortcut_for_an_unknown_action_is_ignored() {
        // A binding written by a newer Umber for a command this one does not
        // have. It must not disturb the table it does have.
        let prefs = from_text("shortcut = Kaleidoscope Ctrl+KeyK\n");
        assert_eq!(prefs.shortcuts, shortcuts::defaults());
    }

    #[test]
    fn a_corrupt_chord_restores_the_default_rather_than_unbinding() {
        let prefs = from_text("shortcut = Undo Ctrl+KeyGarbage\n");
        assert!(shortcuts::is_default(&prefs.shortcuts, Action::Undo));
    }

    #[test]
    fn a_file_may_shadow_a_chord_and_is_loaded_as_written() {
        // Two actions on one chord cannot be produced by the settings page, but
        // a hand-edited file can say it. Loading has to preserve it so the page
        // can show the clash — quietly dropping one would hide the problem.
        let prefs = from_text(concat!(
            "shortcut = PanTool Ctrl+KeyZ\n",
            "shortcut = Undo Ctrl+KeyZ\n",
        ));
        assert_eq!(
            shortcuts::shadowed(&prefs.shortcuts),
            vec![Chord::new(KeyCode::KeyZ, true, false, false)]
        );
    }

    #[test]
    fn the_written_file_parses_as_comments_and_settings_only() {
        // Guards the format itself: every non-comment line has to be a
        // `key = value`, or the parser silently drops half the file.
        let text = to_text(&Prefs::default());
        for line in text.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            assert!(line.contains(" = "), "unparseable line: {line}");
        }
    }
}
