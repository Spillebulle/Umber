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

use crate::autosave;
use crate::colorpicker::{PickerMode, WheelAngles, WheelShape};
use crate::editor::Editor;
use crate::shortcuts::{self, Action, Binding, Chord};
use crate::theme::{Accent, ThemeKind};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
#[cfg(test)]
use std::sync::{Mutex, MutexGuard};
use umber_core::Harmony;
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

/// Bounds on the undo memory budget, in megabytes **per document**.
///
/// The floor is not zero, and could not usefully be: a patch is the whole
/// rectangle a stroke damaged, so below about this a single broad stroke on an
/// ordinary canvas is the entire history and undo stops being worth having.
///
/// **The ceiling is 32 GB, and it is a statement about what the engine can use
/// rather than about what a machine has.** It was 4 GB, which was too low the
/// moment somebody with 64 GB of memory asked for more, and the honest question
/// is not "how much memory is there" — Umber cannot read that, and a per-document
/// figure taken from it would be wrong the moment a second tab opened — but "at
/// what point does a bigger number stop buying depth". A patch is the rectangle
/// a stroke covered, so its cost follows the canvas: on the largest canvas Umber
/// paints, 10000², a stroke drawn across the picture is 400 MB, and 32 GB is
/// eighty of them. On an ordinary 2048² canvas a full-canvas stroke is 16 MB,
/// and 32 GB is two thousand entries — past anything a session produces, so the
/// budget has stopped being what limits the history and the canvas has taken
/// over. Above this the answer is a patch that stores *tiles* rather than the
/// stroke's bounding box, which is what "Undo" in `CLAUDE.md` already says, and
/// not a larger number here.
///
/// It is still **per document**, so two tabs at the ceiling is 64 GB — the whole
/// of the machine the request came from — which is why the note under the
/// control says so rather than letting the figure read as free depth.
pub const MIN_UNDO_BUDGET_MB: u32 = 64;
pub const MAX_UNDO_BUDGET_MB: u32 = 32768;

/// The budgets the dialog's rail lands on, in megabytes.
///
/// A ladder rather than a free slider, like the autosave's expiry: the useful
/// answers are doublings, and nobody is trying to land on 813 MB by dragging.
/// The preferences file still takes any number in range, and so does the figure
/// beside the rail — which is what makes a value between two rungs honoured
/// rather than snapped.
pub const UNDO_BUDGET_LADDER: [u32; 10] = [64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768];

/// A number of megabytes as the history counts bytes.
///
/// Saturating rather than plain, because [`MAX_UNDO_BUDGET_MB`] is 32768 and
/// `32768 << 20` does not fit a 32-bit `usize`. Every target Umber ships is
/// 64-bit, so this cannot fire today; it is one word against a build for a
/// target where the arithmetic would panic in debug and wrap in release.
pub fn undo_budget_bytes(megabytes: u32) -> usize {
    (megabytes as usize).saturating_mul(1024 * 1024)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Prefs {
    pub theme: ThemeKind,
    pub accent: Accent,
    /// The id of a theme in the user's own library, when one is in use.
    ///
    /// Kept *beside* `theme` rather than instead of it, and both are always
    /// written. A custom theme is a file in a directory an update never
    /// touches, but it is also a file somebody can delete, move or fail to
    /// copy onto their next machine — and a preferences file that named only a
    /// theme that has gone would leave the interface with no colours at all.
    /// `theme` is what it falls back to, which is the built-in the custom one
    /// was made from.
    ///
    /// An id is read back exactly as written, with no check that it names
    /// anything — this module cannot see the library. It is [`apply`] that
    /// tries to resolve it, and an id that does not resolve leaves
    /// `Editor::custom_theme` empty, so the *next* thing to write the file
    /// drops the line. That is deliberate rather than tidy-minded: the theme
    /// is genuinely not there, and a preferences file that went on naming it
    /// would mean an interface that changed colour whenever the file came
    /// back.
    pub custom_theme: Option<String>,
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
    /// Which relation the Harmony picker mode shows. Kept for the reason
    /// [`Prefs::picker`] is: it is a way of working, not something to choose
    /// again every morning.
    pub harmony: Harmony,
    /// How far each wheel centre is turned from its neutral pose, in degrees.
    ///
    /// Two keys rather than one, for the reason [`WheelAngles`] gives: the two
    /// shapes do not share a neutral, and an angle is a choice about a shape.
    /// Both are absent from a file written before the setting existed, which
    /// gives the pose those builds drew.
    pub wheel_angles: WheelAngles,
    /// Whether a saved document carries its undo history.
    pub save_history: bool,
    /// A directory of the user's own fonts, scanned beside the machine's own.
    ///
    /// The third of `umber_core::fonts`' three sources, and the one somebody
    /// with a foundry licence or a work library needs. Umber **reads** it and
    /// copies nothing out of it: the moment it copied a face it would be
    /// redistributing one, in somebody's own documents folder.
    ///
    /// A path rather than a list, because one folder is the whole feature and a
    /// list is a management interface. Empty means none, and a folder that is
    /// no longer there is simply scanned as nothing rather than being an error
    /// somebody has to clear before they can set a caption.
    pub font_folder: Option<PathBuf>,
    /// How much memory one document's undo history may hold, in megabytes.
    ///
    /// Per document, not per session — see [`MIN_UNDO_BUDGET_MB`]. Stored in
    /// megabytes because that is the unit the dialog says it in and the unit a
    /// hand-edited file wants; the history counts bytes.
    pub undo_budget_mb: u32,
    /// Whether open documents are written out on a timer at all.
    pub autosave: bool,
    /// How often, in minutes.
    pub autosave_interval_minutes: u32,
    /// How long an autosave's *internal* copy is kept, in hours. Zero is "keep
    /// for ever".
    ///
    /// Hours rather than days because the useful short answers — six hours, a
    /// day — are not whole days, and a duration in one unit is one number to
    /// parse rather than a number and a unit that can disagree. The dialog
    /// still says "30 days" where that is what it means.
    ///
    /// It governs the internal copies and **nothing else**. No setting here can
    /// reach a file the painter chose the location of — see
    /// [`autosave::Reaper`].
    pub autosave_expiry_hours: u32,
    /// The complete binding table, already merged with the defaults.
    pub shortcuts: Vec<Binding>,
}

impl Default for Prefs {
    fn default() -> Self {
        let pressure = umber_core::input::PressureModel::default();
        Self {
            theme: ThemeKind::Graphite,
            accent: Accent::Umber,
            custom_theme: None,
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
            harmony: Harmony::default(),
            wheel_angles: WheelAngles::default(),
            save_history: true,
            font_folder: None,
            // Exactly what every build before the setting existed held, so a
            // missing or older preferences file changes nobody's behaviour.
            undo_budget_mb: (umber_core::history::DEFAULT_BUDGET_BYTES / (1024 * 1024)) as u32,
            autosave: true,
            autosave_interval_minutes: autosave::DEFAULT_INTERVAL_MINUTES,
            autosave_expiry_hours: autosave::DEFAULT_EXPIRY_HOURS,
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
    if let Some(label) = LABEL.get() {
        return label.clone();
    }
    match config_path() {
        Some(path) => path.display().to_string(),
        None => "unavailable: this system has no configuration directory".to_string(),
    }
}

/// What the settings footer shows instead of the real path.
///
/// Set only by `docshot`, and the reason is that [`config_path`] names the
/// account the process is running as. A committed picture of the settings dialog
/// would otherwise carry a developer's home directory into the README, and would
/// come back different for every contributor who regenerated it — a diff nobody
/// could review, over a detail nobody meant to publish. Nothing in the
/// application calls this, and `set_config_path_label` is the only way in.
static LABEL: OnceLock<String> = OnceLock::new();

/// Show `label` in place of the real preferences path, for pictures of the
/// dialog. Takes effect once and cannot be undone.
pub fn set_config_path_label(label: &str) {
    let _ = LABEL.set(label.to_owned());
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
    // Absent rather than empty when there is none: an empty value would be a
    // theme whose id is the empty string, which `ThemeLibrary::path_of` would
    // slug into a file called `theme`.
    if let Some(id) = &prefs.custom_theme {
        out.push_str(&format!("custom_theme = {id}\n"));
    }
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
    out.push_str(&format!("harmony = {}\n", harmony_id(prefs.harmony)));
    for shape in WheelShape::ALL {
        out.push_str(&format!(
            "{} = {:.1}\n",
            wheel_angle_key(shape),
            prefs.wheel_angles.of(shape)
        ));
    }
    out.push_str(&format!("save_history = {}\n", prefs.save_history));
    // Written only when there is one, so a preferences file from a session that
    // never set it is byte for byte what it was before this key existed.
    if let Some(folder) = &prefs.font_folder {
        out.push_str(&format!("font_folder = {}\n", folder.display()));
    }
    out.push_str(&format!("undo_budget_mb = {}\n", prefs.undo_budget_mb));
    out.push_str(&format!("autosave = {}\n", prefs.autosave));
    out.push_str(&format!(
        "autosave_interval_minutes = {}\n",
        prefs.autosave_interval_minutes
    ));
    out.push_str(&format!(
        "autosave_expiry_hours = {}\n",
        prefs.autosave_expiry_hours
    ));

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
            // Kept as written, with no check that it names anything: the
            // library is a directory this module does not read, and a theme
            // temporarily out of reach — a data directory on a drive that is
            // not mounted yet — must not have its name struck from the file.
            // `Editor::palette` is where an id that names nothing falls back to
            // the built-in beside it.
            "custom_theme" => {
                let id = value.trim();
                if !id.is_empty() {
                    prefs.custom_theme = Some(id.to_owned());
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
            "harmony" => {
                if let Some(v) = harmony_from_id(value) {
                    prefs.harmony = v;
                }
            }
            // Wrapped rather than clamped, unlike every other number here: an
            // angle's range is a period, so a hand-edited 400 means 40 rather
            // than "as far round as the slider goes". `WheelAngles::set` is the
            // one door that does it, so the file, the slider and the default
            // cannot disagree — and it is also what refuses a NaN, which would
            // otherwise reach `sin_cos` and take the picker's mesh with it.
            "wheel_triangle_angle" | "wheel_square_angle" => {
                if let Some(shape) = WheelShape::ALL
                    .into_iter()
                    .find(|s| wheel_angle_key(*s) == key)
                    && let Some(v) = parse_f32(value, -MAX_TURNS, MAX_TURNS)
                {
                    prefs.wheel_angles.set(shape, v);
                }
            }
            "save_history" => {
                if let Some(v) = parse_bool(value) {
                    prefs.save_history = v;
                }
            }
            // Taken verbatim, and deliberately not checked for existence here:
            // a removable drive that is not plugged in today is still the
            // folder somebody chose, and clearing the setting because of it
            // would be Umber quietly forgetting a choice. A folder that is not
            // there scans as nothing.
            "font_folder" => {
                if !value.is_empty() {
                    prefs.font_folder = Some(PathBuf::from(value));
                }
            }
            // Clamped like every other number here. A line that cannot be read
            // leaves the shipped 512 MB in place, which is the direction that
            // costs memory rather than somebody's undo history.
            "undo_budget_mb" => {
                if let Some(v) = parse_u32(value, MIN_UNDO_BUDGET_MB, MAX_UNDO_BUDGET_MB) {
                    prefs.undo_budget_mb = v;
                }
            }
            "autosave" => {
                if let Some(v) = parse_bool(value) {
                    prefs.autosave = v;
                }
            }
            "autosave_interval_minutes" => {
                if let Some(v) = parse_u32(
                    value,
                    autosave::MIN_INTERVAL_MINUTES,
                    autosave::MAX_INTERVAL_MINUTES,
                ) {
                    prefs.autosave_interval_minutes = v;
                }
            }
            // Clamped, not rejected, like every other number here — but the
            // floor is zero, which is "keep for ever". A hand-edited value that
            // does not parse leaves the default in place; it can never turn
            // into a *shorter* expiry by accident, which is the direction that
            // would delete something.
            "autosave_expiry_hours" => {
                if let Some(v) = parse_u32(value, 0, autosave::MAX_EXPIRY_HOURS) {
                    prefs.autosave_expiry_hours = v;
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

/// Parse a whole number and clamp it into range, as [`parse_f32`] does.
fn parse_u32(value: &str, lo: u32, hi: u32) -> Option<u32> {
    value.parse::<u32>().ok().map(|v| v.clamp(lo, hi))
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
        PickerMode::Harmony => "harmony",
    }
}

/// Stable names for the harmony relations, for the reason [`picker_id`] gives.
///
/// A `match` rather than a slug of the label, so adding a relation is a missing
/// arm rather than a name that changes silently the day the label is reworded.
fn harmony_id(harmony: Harmony) -> &'static str {
    match harmony {
        Harmony::Complementary => "complementary",
        Harmony::Analogous => "analogous",
        Harmony::Triad => "triad",
        Harmony::SplitComplementary => "split-complementary",
        Harmony::Tetrad => "tetrad",
    }
}

fn harmony_from_id(id: &str) -> Option<Harmony> {
    Harmony::ALL.into_iter().find(|h| harmony_id(*h) == id)
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

/// The key each shape's angle is written under, for the same reason
/// [`wheel_shape_id`] is a `match`: adding a third centre has to be a decision
/// about what a file written a year from now will call it.
fn wheel_angle_key(shape: WheelShape) -> &'static str {
    match shape {
        WheelShape::Triangle => "wheel_triangle_angle",
        WheelShape::Square => "wheel_square_angle",
    }
}

/// How far out of a single turn a hand-edited angle is still read.
///
/// [`parse_f32`] clamps, and an angle wants wrapping — so the bound is only
/// there to keep a number large enough to lose its low bits away from
/// `rem_euclid`. Anything inside it wraps exactly.
const MAX_TURNS: f32 = 360.0 * 1000.0;

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
        custom_theme: ed.custom_theme.as_ref().map(|t| t.id.clone()),
        interface_scale: ctx.zoom_factor(),
        pressure_source: ed.pressure.source,
        pressure_max_speed: ed.pressure.max_speed,
        pressure_response: ed.pressure.responsiveness,
        check_updates: ed.updates.check_on_startup,
        update_notice_seen: ed.updates.notice_seen,
        wheel_rotates: ed.ui.wheel_rotates,
        picker: ed.ui.picker,
        wheel_shape: ed.ui.wheel_shape,
        harmony: ed.ui.harmony,
        wheel_angles: ed.ui.wheel_angles,
        save_history: ed.ui.save_history,
        font_folder: ed.font_folder.clone(),
        // Read off the live history, like every other value here is read off
        // the thing that uses it, so the file cannot come to disagree with what
        // the running documents are actually held to.
        undo_budget_mb: (ed.history.budget_bytes() / (1024 * 1024)) as u32,
        autosave: ed.autosave.enabled,
        autosave_interval_minutes: (ed.autosave.interval.as_secs() / 60).max(1) as u32,
        autosave_expiry_hours: ed
            .autosave
            .expiry
            .map(|d| (d.as_secs() / 3600) as u32)
            .unwrap_or(0),
        shortcuts: shortcuts::published(),
    }
}

/// Point the font scan at a folder of the user's own, or at none.
///
/// One door, because changing it has to **forget the scan** as well as record
/// the choice: a library still holding the old folder's faces would go on
/// offering faces the artist has just pointed Umber away from, and the picker
/// would then resolve a name to a file that is no longer in the search. Setting
/// it to what it already is does nothing, so applying preferences at start-up
/// does not throw away a scan that has just landed.
pub fn set_font_folder(ed: &mut Editor, folder: Option<PathBuf>) {
    if ed.font_folder == folder {
        return;
    }
    ed.font_folder = folder;
    ed.text.fonts.forget();
}

/// Hand a new undo budget to the running application.
///
/// Two halves, because they answer different questions, and one door so that
/// neither call site can do only one of them. The published value is what a
/// document opened from here on takes — a blank canvas, a new tab, an import —
/// none of which can see a `Prefs`. The second reaches the document being
/// edited *now*, and it is what makes turning the limit down give the memory
/// back at once rather than at the next stroke.
///
/// The documents parked in other tabs are reached too, and have to be: the
/// budget is *per document*, so a session of four tabs is four ceilings, and
/// one lowered to give memory back that left three untouched would give back a
/// quarter of what the dialog said.
pub fn set_undo_budget(ed: &mut Editor, megabytes: u32) {
    let bytes = undo_budget_bytes(megabytes);
    umber_core::history::set_default_budget(bytes);
    ed.history.set_budget(bytes);
    for i in 0..ed.session.len() {
        if let Some(state) = ed.session.parked_mut(i) {
            state.history.set_budget(bytes);
        }
    }
}

/// Push stored preferences into the running app.
pub fn apply(prefs: &Prefs, ctx: &egui::Context, ed: &mut Editor) {
    ed.ui.theme = prefs.theme;
    ed.ui.accent = prefs.accent;
    // The library is only read when a custom theme is actually named, so the
    // ordinary path — and every test in this file — touches no disk. An id that
    // names nothing leaves `custom_theme` empty, which is `Editor::palette`'s
    // fallback to the built-in `theme` also names.
    ed.custom_theme = prefs.custom_theme.as_deref().and_then(|id| {
        crate::themelib::ThemeLibrary::load()
            .ok()
            .and_then(|library| library.get(id).cloned())
    });
    ed.pressure.source = prefs.pressure_source;
    ed.pressure.max_speed = prefs.pressure_max_speed;
    ed.pressure.responsiveness = prefs.pressure_response;
    ed.updates.check_on_startup = prefs.check_updates;
    ed.updates.notice_seen = prefs.update_notice_seen;
    ed.ui.wheel_rotates = prefs.wheel_rotates;
    ed.ui.picker = prefs.picker;
    ed.ui.wheel_shape = prefs.wheel_shape;
    ed.ui.harmony = prefs.harmony;
    ed.ui.wheel_angles = prefs.wheel_angles;
    ed.ui.save_history = prefs.save_history;
    set_font_folder(ed, prefs.font_folder.clone());
    set_undo_budget(ed, prefs.undo_budget_mb);
    ed.autosave.enabled = prefs.autosave;
    ed.autosave.interval =
        std::time::Duration::from_secs(prefs.autosave_interval_minutes.max(1) as u64 * 60);
    // Zero hours is "keep for ever", which is `None` — not a zero-length
    // expiry, which would delete every internal copy the moment it was written.
    ed.autosave.expiry = (prefs.autosave_expiry_hours > 0)
        .then(|| std::time::Duration::from_secs(prefs.autosave_expiry_hours as u64 * 3600));
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

/// The right to be the only test writing the undo budget's process-global.
///
/// `apply` and [`set_undo_budget`] both publish to
/// `umber_core::history::set_default_budget`, which is deliberate — a `History`
/// is built by three things that cannot see a `Prefs`, so the setting reaches
/// them the way `shortcuts::publish` does. The cost is that every test touching
/// either writes one variable, and the harness runs them on parallel threads.
///
/// Measured before it was fixed: `the_undo_budget_reaches_the_history_and_back`
/// failed **10 runs in 40** at sixteen threads. It publishes 1024 MB and asserts
/// it; three other tests publish the default and land between the two lines. It
/// survived the whole-workspace run because six hundred other tests change the
/// interleaving, which is the worst shape for this — green on the gate, red on
/// whoever next runs `cargo test prefs`.
///
/// Hold the guard for the **whole** test; binding it with `let _ =` drops it on
/// the spot and buys nothing. Poisoning is recovered from so one failing test
/// reports its own assertion rather than turning every later one into a mutex
/// error — `gputest::lock`'s reasoning, and this is that idiom for a global that
/// is not a device.
///
/// It is `pub(crate)` and not private to this module's tests because
/// `settings`' undo-budget row writes the same global through the same door, and
/// **two mutexes would serialise nothing**: a lock only orders the tests that
/// take the same one.
#[cfg(test)]
pub(crate) fn prefs_lock() -> MutexGuard<'static, ()> {
    static SERIAL: Mutex<()> = Mutex::new(());
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    use winit::keyboard::KeyCode;

    fn turned(triangle: f32, square: f32) -> WheelAngles {
        let mut angles = WheelAngles::default();
        angles.set(WheelShape::Triangle, triangle);
        angles.set(WheelShape::Square, square);
        angles
    }

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
        assert_eq!(prefs.wheel_angles, editor.ui.wheel_angles);
        assert_eq!(prefs.save_history, editor.ui.save_history);
        // Against the constant rather than against `editor.history`, which
        // reads a published value another test in this process may have moved.
        // What has to hold is that the shipped default is the figure every
        // build before the setting existed used.
        assert_eq!(
            undo_budget_bytes(prefs.undo_budget_mb),
            umber_core::history::DEFAULT_BUDGET_BYTES
        );
        assert_eq!(prefs.autosave, editor.autosave.enabled);
        assert_eq!(
            prefs.autosave_interval_minutes as u64 * 60,
            editor.autosave.interval.as_secs()
        );
        assert_eq!(
            prefs.autosave_expiry_hours as u64 * 3600,
            editor.autosave.expiry.map(|d| d.as_secs()).unwrap_or(0)
        );
        assert_eq!(prefs.shortcuts, shortcuts::defaults());
    }

    /// The one setting whose control is not in the settings dialog. It is set
    /// on the Colour panel, under the wheel it applies to, and it has to be
    /// carried through the file and back into the editor exactly like the ones
    /// that are — which is the step that would be easy to leave out.
    #[test]
    fn the_wheels_rotation_survives_a_restart() {
        let _serial = prefs_lock();
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

    /// Every relation, for the reason `harmony_id`'s own comment gives: the ids
    /// are a `match` so that adding a relation is a missing arm, and this is
    /// what says the arm somebody adds actually round-trips.
    #[test]
    fn every_harmony_relation_survives_a_restart() {
        for harmony in Harmony::ALL {
            let prefs = Prefs {
                harmony,
                ..Prefs::default()
            };
            let back = from_text(&to_text(&prefs));
            assert_eq!(back.harmony, harmony, "{}", harmony.label());
        }
        // The ids are the file's own words and are deliberately not the labels
        // lower-cased, so a label reworded tomorrow cannot silently change what
        // a file written today means.
        assert_eq!(
            from_text("harmony = split-complementary\n").harmony,
            Harmony::SplitComplementary
        );
        assert_eq!(
            from_text("harmony = Split complementary\n").harmony,
            Harmony::default(),
            "a bad id keeps the default"
        );
    }

    /// The angle is set on the Colour panel, like the rotation above it, and has
    /// to reach the picker rather than only the `Prefs` struct — and each shape
    /// has to arrive with its own number, since that is the whole point of
    /// keeping two.
    #[test]
    fn each_wheel_shapes_angle_survives_a_restart() {
        let _serial = prefs_lock();
        let prefs = Prefs {
            wheel_angles: turned(30.0, 200.0),
            ..Prefs::default()
        };
        let back = from_text(&to_text(&prefs));
        assert_eq!(back.wheel_angles.of(WheelShape::Triangle), 30.0);
        assert_eq!(back.wheel_angles.of(WheelShape::Square), 200.0);

        let ctx = egui::Context::default();
        let mut editor = Editor::default();
        apply(&back, &ctx, &mut editor);
        assert_eq!(
            editor.ui.wheel_angles, back.wheel_angles,
            "reading the file must reach the picker, not just the Prefs struct"
        );
        assert_eq!(capture(&ctx, &editor).wheel_angles, back.wheel_angles);
    }

    /// A file written before the setting existed says nothing about it, and must
    /// therefore draw both centres exactly where that build drew them.
    #[test]
    fn a_file_without_an_angle_leaves_both_centres_where_they_were() {
        let prefs = from_text("version = 1\npicker = wheel\nwheel_shape = square\n");
        assert_eq!(prefs.wheel_angles, WheelAngles::default());
        assert_eq!(prefs.wheel_angles.of(WheelShape::Triangle), 0.0);
        assert_eq!(prefs.wheel_angles.of(WheelShape::Square), 0.0);
    }

    /// An angle's range is a period, not a limit, so a hand-edited number is
    /// wrapped where every other value here is clamped. The one that must not
    /// get through is a NaN: it would reach `sin_cos` and take the picker's mesh
    /// with it.
    #[test]
    fn a_hand_edited_angle_is_wrapped_rather_than_clamped() {
        let prefs = from_text(concat!(
            "wheel_triangle_angle = 400\n",
            "wheel_square_angle = -30\n",
        ));
        assert_eq!(prefs.wheel_angles.of(WheelShape::Triangle), 40.0);
        assert_eq!(prefs.wheel_angles.of(WheelShape::Square), 330.0);

        for bad in ["NaN", "inf", "", "sideways"] {
            let prefs = from_text(&format!("wheel_triangle_angle = {bad}\n"));
            assert_eq!(
                prefs.wheel_angles.of(WheelShape::Triangle),
                0.0,
                "{bad} must leave the default in place"
            );
        }
    }

    /// The two keys are separate, and one must never be read as the other — the
    /// same trap `picker` and `wheel_shape` share.
    #[test]
    fn the_two_angle_keys_do_not_read_each_other() {
        let prefs = from_text("wheel_triangle_angle = 90\n");
        assert_eq!(prefs.wheel_angles.of(WheelShape::Triangle), 90.0);
        assert_eq!(prefs.wheel_angles.of(WheelShape::Square), 0.0);
    }

    /// A theme somebody made is named by id, and the built-in it was made from
    /// is written *beside* it rather than instead of it — the file that names a
    /// theme which has since been deleted must still say what to fall back to,
    /// or the interface has no colours at all.
    #[test]
    fn a_theme_somebody_made_survives_a_restart_and_keeps_its_fallback() {
        let prefs = Prefs {
            theme: ThemeKind::Paper,
            custom_theme: Some("midnight-oil".to_owned()),
            ..Prefs::default()
        };
        let text = to_text(&prefs);
        assert!(text.contains("custom_theme = midnight-oil\n"));
        assert!(text.contains("theme = paper\n"), "the fallback goes too");
        let back = from_text(&text);
        assert_eq!(back.custom_theme.as_deref(), Some("midnight-oil"));
        assert_eq!(back.theme, ThemeKind::Paper);

        // Nothing written at all with no custom theme, so a file from a build
        // before this existed and a file written by this one are the same file.
        let plain = to_text(&Prefs::default());
        assert!(!plain.contains("custom_theme"));
        assert_eq!(from_text(&plain).custom_theme, None);
        // And an empty value is not an id: `ThemeLibrary::path_of` would slug
        // the empty string into a file called `theme`.
        assert_eq!(from_text("custom_theme =   \n").custom_theme, None);
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

    /// The undo budget has to reach the *history*, not only the `Prefs` struct,
    /// and come back out of it again — otherwise the dialog would show a figure
    /// no document was ever held to.
    ///
    /// Raised rather than lowered, and put back: `apply` publishes the value for
    /// every history built afterwards, so a smaller ceiling left behind here
    /// would arrive underneath another test's document.
    #[test]
    fn the_undo_budget_reaches_the_history_and_back() {
        let _serial = prefs_lock();
        let prefs = Prefs {
            undo_budget_mb: 1024,
            ..Prefs::default()
        };
        let back = from_text(&to_text(&prefs));
        assert_eq!(back.undo_budget_mb, 1024);

        let ctx = egui::Context::default();
        let mut editor = Editor::default();
        apply(&back, &ctx, &mut editor);
        assert_eq!(
            editor.history.budget_bytes(),
            undo_budget_bytes(1024),
            "the setting must reach the document being edited"
        );
        assert_eq!(
            umber_core::history::default_budget(),
            undo_budget_bytes(1024),
            "and the document opened next"
        );
        assert_eq!(capture(&ctx, &editor).undo_budget_mb, 1024);

        apply(&Prefs::default(), &ctx, &mut editor);
        assert_eq!(
            umber_core::history::default_budget(),
            umber_core::history::DEFAULT_BUDGET_BYTES
        );
    }

    /// A hand-edited budget is clamped into the range the dialog offers, and a
    /// line that cannot be read leaves the shipped figure in place — the
    /// direction that costs memory rather than somebody's undo history.
    #[test]
    fn a_hand_edited_undo_budget_is_clamped_rather_than_dropped() {
        assert_eq!(
            from_text("undo_budget_mb = 1\n").undo_budget_mb,
            MIN_UNDO_BUDGET_MB
        );
        assert_eq!(
            from_text("undo_budget_mb = 999999\n").undo_budget_mb,
            MAX_UNDO_BUDGET_MB
        );
        assert_eq!(
            from_text("undo_budget_mb = plenty\n").undo_budget_mb,
            Prefs::default().undo_budget_mb
        );
        // The dialog's ladder and the file's range are two statements of the
        // same bounds, and a dialog offering a value the file would clamp would
        // be a control that lies about what it set.
        assert_eq!(UNDO_BUDGET_LADDER[0], MIN_UNDO_BUDGET_MB);
        assert_eq!(
            *UNDO_BUDGET_LADDER.last().unwrap(),
            MAX_UNDO_BUDGET_MB,
            "the ladder must reach the top of the range"
        );
        assert!(
            UNDO_BUDGET_LADDER.contains(&Prefs::default().undo_budget_mb),
            "the shipped default has to be a rung, or the slider cannot show it"
        );
    }

    /// Every rung is twice the one below it, starting at the floor.
    ///
    /// Not decoration. `settings::budget_position` turns a budget into a place
    /// on the rail with a `log2` rather than a search, which is only the same
    /// answer while this holds; insert 768 MB between two rungs and the knob
    /// would sit somewhere the readout disagrees with, silently, on a control
    /// whose whole job is to say what it set. Nothing about the constant's type
    /// says it is a geometric series, so it is said here.
    #[test]
    fn the_undo_budget_ladder_is_doublings_from_the_floor() {
        for (i, rung) in UNDO_BUDGET_LADDER.iter().enumerate() {
            assert_eq!(
                *rung,
                MIN_UNDO_BUDGET_MB << i,
                "rung {i} is not the floor doubled {i} times"
            );
        }
    }

    /// The autosave's settings, and — the part that matters — which way each of
    /// them fails.
    #[test]
    fn a_corrupt_autosave_setting_never_shortens_the_expiry() {
        let prefs = from_text(concat!(
            "autosave = sometimes\n",
            "autosave_interval_minutes = soon\n",
            "autosave_expiry_hours = -1\n",
        ));
        assert!(
            prefs.autosave,
            "a line that cannot be read must not stop it"
        );
        assert_eq!(
            prefs.autosave_interval_minutes,
            autosave::DEFAULT_INTERVAL_MINUTES
        );
        // The direction that matters: a value that cannot be read must never
        // become a *shorter* expiry, because a shorter expiry deletes things.
        assert_eq!(prefs.autosave_expiry_hours, autosave::DEFAULT_EXPIRY_HOURS);
    }

    #[test]
    fn the_autosave_settings_survive_a_restart() {
        let _serial = prefs_lock();
        let prefs = Prefs {
            autosave: false,
            autosave_interval_minutes: 20,
            // Zero is "keep for ever", and has to survive as itself rather than
            // being clamped up to the minimum interval or down to nothing.
            autosave_expiry_hours: 0,
            ..Prefs::default()
        };
        let back = from_text(&to_text(&prefs));
        assert!(!back.autosave);
        assert_eq!(back.autosave_interval_minutes, 20);
        assert_eq!(back.autosave_expiry_hours, 0);

        let ctx = egui::Context::default();
        let mut editor = Editor::default();
        apply(&back, &ctx, &mut editor);
        assert!(!editor.autosave.enabled);
        assert_eq!(editor.autosave.interval.as_secs(), 20 * 60);
        assert_eq!(
            editor.autosave.expiry, None,
            "zero hours must be “keep for ever”, not an expiry of zero"
        );
        assert_eq!(capture(&ctx, &editor).autosave_expiry_hours, 0);
    }

    #[test]
    fn a_hand_edited_interval_is_clamped_rather_than_dropped() {
        assert_eq!(
            from_text("autosave_interval_minutes = 0\n").autosave_interval_minutes,
            autosave::MIN_INTERVAL_MINUTES,
        );
        assert_eq!(
            from_text("autosave_interval_minutes = 99999\n").autosave_interval_minutes,
            autosave::MAX_INTERVAL_MINUTES,
        );
        // Any number of hours the file names is honoured, not snapped to the
        // ladder the dialog offers.
        assert_eq!(
            from_text("autosave_expiry_hours = 100\n").autosave_expiry_hours,
            100
        );
    }

    #[test]
    fn a_full_round_trip_preserves_everything() {
        let mut prefs = Prefs {
            theme: ThemeKind::Paper,
            accent: Accent::Clay,
            custom_theme: Some("midnight-oil".to_owned()),
            interface_scale: 1.25,
            pressure_source: PressureSource::Simulated,
            pressure_max_speed: 1800.0,
            pressure_response: 0.6,
            check_updates: false,
            update_notice_seen: true,
            wheel_rotates: false,
            harmony: Harmony::Tetrad,
            picker: PickerMode::Sliders,
            wheel_shape: WheelShape::Square,
            wheel_angles: turned(30.0, 200.0),
            save_history: false,
            font_folder: Some(PathBuf::from("/home/painter/type/My Foundry")),
            undo_budget_mb: 1024,
            autosave: false,
            autosave_interval_minutes: 12,
            autosave_expiry_hours: 48,
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
        assert_eq!(back.custom_theme, prefs.custom_theme);
        assert_eq!(back.interface_scale, prefs.interface_scale);
        assert_eq!(back.pressure_source, prefs.pressure_source);
        assert_eq!(back.pressure_max_speed, prefs.pressure_max_speed);
        assert_eq!(back.pressure_response, prefs.pressure_response);
        assert_eq!(back.check_updates, prefs.check_updates);
        assert_eq!(back.update_notice_seen, prefs.update_notice_seen);
        assert_eq!(back.wheel_rotates, prefs.wheel_rotates);
        assert_eq!(back.picker, prefs.picker);
        assert_eq!(back.wheel_shape, prefs.wheel_shape);
        assert_eq!(back.wheel_angles, prefs.wheel_angles);
        assert_eq!(back.save_history, prefs.save_history);
        // A path with a space in it, because the file is `key = value` split on
        // the first `=` and a folder somebody actually has is not one word.
        assert_eq!(back.font_folder, prefs.font_folder);
        assert_eq!(back.undo_budget_mb, prefs.undo_budget_mb);
        assert_eq!(back.autosave, prefs.autosave);
        assert_eq!(
            back.autosave_interval_minutes,
            prefs.autosave_interval_minutes
        );
        assert_eq!(back.autosave_expiry_hours, prefs.autosave_expiry_hours);
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
