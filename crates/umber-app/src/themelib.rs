//! Themes the user makes: what one is, what it is written as, and the
//! directory of them on disk.
//!
//! There is no drawing in here at all — `settings.rs`'s Themes pane paints it,
//! this decides what a theme is, whether a name is free, and what import and
//! export read and write. Same division `dock.rs` keeps against `panels.rs`,
//! and it is what lets the whole of the format be tested without a window.
//!
//! # A user's theme is a [`Palette`] and nothing more
//!
//! That is the whole reason the second theme was a table of values rather than
//! an edit sweep: every colour the interface draws comes out of `Palette`, so
//! a theme somebody makes needs no new mechanism, no branch anywhere that
//! draws, and no second door into egui's styling — it goes through
//! [`crate::theme::apply`] exactly as Graphite and Paper do. [`CustomTheme`]
//! is a `Palette`, a name, and the built-in it started from; there is nothing
//! else in it.
//!
//! # Why it lives in `umber-app`
//!
//! `umber_core::palette` is the model for the *artist's* palettes and is in
//! core because it holds plain bytes. A theme holds `egui::Color32`, and
//! `umber-core` may not depend on egui — the boundary that keeps the engine
//! testable without a window. So the theme library is here, beside the
//! `Palette` it is made of, and it is still a model with no drawing in it.
//!
//! # Why it is a directory of files
//!
//! The same three reasons `umber_core::palette` gives, and the one that
//! matters most applies unchanged: **the interchange format is the storage
//! format.** Import is bringing a file into the directory and export is
//! copying one out, so there is one encoder and one decoder rather than a
//! stored form and a shared form that can drift — the rule `docformat` states
//! as "there must never be a second ORA reader". A write touches one small
//! file rather than an index holding every theme, and the files are ordinary
//! files somebody can hand to somebody else.
//!
//! The format is `prefs`'s: a header line, then flat `key = value`. It needs no
//! dependency, it is hand-editable, and — the reason that matters — a line that
//! does not parse costs that one token and nothing else, which is exactly the
//! tolerance a file written by a different build of Umber needs.
//!
//! # `base` is what an absent token falls back to
//!
//! A file names the built-in theme it was made from, and any token it does not
//! carry takes that theme's value. Two things follow, and both were the reason:
//! a theme written by an older Umber still gets a sensible colour for a token
//! added since — from the *nearest* built-in rather than from Graphite, so a
//! light theme does not acquire a near-black surface — and a file somebody has
//! hand-trimmed to the six colours they cared about is a legal theme rather
//! than a mostly-black one. Every token is nevertheless always written, so what
//! leaves Umber is complete and legible.
//!
//! # Nothing is shipped
//!
//! Graphite and Paper are compiled in and are not entries in this library, for
//! the reason `umber_core::palette`'s module docs give in full: anything the
//! user decides about a shipped item cannot be written where the shipped item
//! is, because an update replaces it wholesale and the choice vanishes
//! silently, months later. Copying a built-in into the library — which is what
//! [`ThemeLibrary::duplicate`] does, and the only way a custom theme is made —
//! puts the copy in the user's own data directory, which an update never
//! touches.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use egui::Color32;

use crate::theme::{Palette, ThemeKind, Token};

/// The extension every theme file carries, and the only one the library reads.
pub const EXTENSION: &str = "umbertheme";

/// The line every theme file starts with.
///
/// A header rather than trusting the extension: `import` is handed whatever the
/// file dialog returned, and a text file that is not a theme has to be refused
/// with a sentence rather than read as a theme of entirely default colours.
const HEADER: &str = "Umber theme";

/// The most themes one library directory may hold.
///
/// It bounds the *parses*, not the listing — [`ThemeLibrary::load_from`] still
/// lists and sorts every path before the cap applies, because every file it
/// finds owns its name whether or not it was read. That is the expensive half
/// only for a directory somebody pointed at something absurd; what this stops
/// is reading and rasterising a card for each. Far past what anybody makes by
/// hand.
pub const MAX_THEMES: usize = 128;

/// What a theme falls back to being called.
pub const UNTITLED: &str = "Untitled theme";

/// The longest name a theme may carry, in characters.
///
/// A bound rather than a design value, and it is enforced on the way *in* as
/// well as on the way out, because a name is the one thing in a theme file that
/// is free text and it ends up on a 150-point card. Without it a hand-edited or
/// truncated file could hand the card row a name the length of the file, which
/// has to be laid out and cut to fit — per card, per frame — and that is work
/// on the drawing path bounded by nothing.
pub const MAX_NAME: usize = 64;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum ThemeError {
    /// The file does not begin with [`HEADER`].
    NotATheme(PathBuf),
    /// The library already holds as many themes as it will read back.
    Full {
        max: usize,
    },
    /// No id in this library matches.
    Unknown(String),
    NoDataDirectory,
    Io {
        path: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for ThemeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotATheme(path) => write!(
                f,
                "{} is not an Umber theme — the first line has to be “{HEADER}”",
                path.display()
            ),
            Self::Full { max } => write!(
                f,
                "your library already holds {max} themes, which is as many as Umber reads back"
            ),
            Self::Unknown(id) => write!(f, "there is no theme called “{id}” in your library"),
            Self::NoDataDirectory => write!(
                f,
                "this system has no user data directory to keep themes in"
            ),
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for ThemeError {}

// ---------------------------------------------------------------------------
// A theme
// ---------------------------------------------------------------------------

/// A theme somebody made: a name, the built-in it started from, and a
/// [`Palette`].
#[derive(Clone, Debug, PartialEq)]
pub struct CustomTheme {
    /// Stable and opaque, and in a [`ThemeLibrary`] it is the file's stem — so
    /// the filesystem keeps it unique and there is no second table to keep in
    /// step. Never derived from the name at read time: renaming a theme must
    /// not orphan the preference pointing at it.
    pub id: String,
    pub name: String,
    /// Which built-in an absent token falls back to. See the module docs.
    pub base: ThemeKind,
    pub palette: Palette,
}

impl CustomTheme {
    /// A copy of a palette, under a name, based on `base`.
    pub fn new(name: impl Into<String>, base: ThemeKind, palette: Palette) -> Self {
        Self {
            id: String::new(),
            name: name.into(),
            base,
            palette,
        }
    }

    /// Serialise. Every token is written, in [`Token::ALL`] order, so what
    /// leaves Umber is complete rather than only the tokens that differ.
    pub fn to_text(&self) -> String {
        let mut out = String::from(HEADER);
        out.push('\n');
        out.push_str("# A theme is a table of colours. Every line is a token and a\n");
        out.push_str("# six-digit sRGB hex; a line that does not parse costs that one\n");
        out.push_str("# colour and nothing else, and a token left out takes the base\n");
        out.push_str("# theme's value.\n");
        out.push_str(&format!("name = {}\n", one_line(&self.name)));
        out.push_str(&format!("base = {}\n", base_id(self.base)));
        for token in Token::ALL {
            out.push_str(&format!(
                "{} = {}\n",
                token.id(),
                hex(self.palette.token(token))
            ));
        }
        out
    }
}

/// `#RRGGBB` for a token. Alpha is deliberately not carried: every palette
/// token is drawn as an opaque fill or an opaque stroke, and the places that
/// want it faded ask for that at the call site with `gamma_multiply`.
pub fn hex(colour: Color32) -> String {
    format!("#{:02X}{:02X}{:02X}", colour.r(), colour.g(), colour.b())
}

/// Read `#RRGGBB`, `RRGGBB` or `#RGB`.
///
/// The short form is accepted because it is what people type; anything else —
/// eight digits, a name, a number — is `None` rather than a guess, and the
/// caller leaves the token it had in place. A theme that quietly took black for
/// a misread line would be a theme with an invisible interface in it.
pub fn parse_hex(text: &str) -> Option<Color32> {
    let body = text.trim().trim_start_matches('#');
    let digits: Vec<u8> = body
        .chars()
        .map(|c| c.to_digit(16).map(|d| d as u8))
        .collect::<Option<Vec<u8>>>()?;
    let [r, g, b] = match digits.len() {
        3 => [digits[0] * 17, digits[1] * 17, digits[2] * 17],
        6 => [
            digits[0] * 16 + digits[1],
            digits[2] * 16 + digits[3],
            digits[4] * 16 + digits[5],
        ],
        _ => return None,
    };
    Some(Color32::from_rgb(r, g, b))
}

fn base_id(kind: ThemeKind) -> &'static str {
    // The same ids `prefs` writes, deliberately: a theme file and a preferences
    // file naming the same built-in by two different words is a thing somebody
    // would eventually have to reconcile by hand.
    match kind {
        ThemeKind::Graphite => "graphite",
        ThemeKind::Paper => "paper",
    }
}

fn base_from_id(id: &str) -> Option<ThemeKind> {
    ThemeKind::ALL.into_iter().find(|k| base_id(*k) == id)
}

/// A name with anything that would break the line format taken out, cut to
/// [`MAX_NAME`].
///
/// Both directions go through this — [`CustomTheme::to_text`] on the way out
/// and [`read_theme`] on the way in — so a file somebody hand-edited is held to
/// the same bound as a name typed into the editor. A control character would
/// otherwise produce a file whose next line is read as a token; an unbounded
/// name is the drawing-path cost [`MAX_NAME`] describes.
fn clean_name(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_NAME)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// [`clean_name`], with the fallback for a name that came out empty.
///
/// Separate, because "" is a real answer on the way *in*: a file whose `name`
/// line is blank falls back to its own filename, the way every `.gpl` reader
/// does, rather than to the word Untitled.
fn one_line(text: &str) -> String {
    match clean_name(text).as_str() {
        "" => UNTITLED.to_owned(),
        trimmed => trimmed.to_owned(),
    }
}

/// Room left under [`MAX_NAME`] for the number `unique_name` appends.
///
/// A desired name is clipped to this before it is made unique, so the suffix
/// can never be the part the cap cuts off — which would put two themes back on
/// one name, the thing `free_name` exists to prevent.
const SUFFIX_ROOM: usize = 8;

fn clipped(desired: &str) -> String {
    // Cleaned as well as cut, so what `unique_name` compares and what the list
    // holds are the string the file will hold. Without it a name carrying a
    // control character sat in memory in one form and on disk in another until
    // the next load — and the uniqueness had been decided against the first.
    clean_name(desired)
        .chars()
        .take(MAX_NAME.saturating_sub(SUFFIX_ROOM))
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// What came of reading a theme file.
#[derive(Clone, Debug)]
pub struct ThemeRead {
    pub theme: CustomTheme,
    /// Lines that were neither a header, a comment, nor a token this build
    /// knows, and lines whose colour would not parse.
    ///
    /// Reported rather than swallowed, for the reason `docimport`'s rule gives:
    /// an import that loses something must say so. A token from a newer Umber
    /// is counted here and the theme still loads, which is the same tolerance
    /// `prefs` gives a key it has never heard of.
    pub skipped: usize,
}

/// Parse a theme file.
///
/// `path` is only used to phrase the error and to fall back for a name, so a
/// caller with the text in hand and no file may pass any name.
pub fn read_theme(text: &str, path: &Path) -> Result<ThemeRead, ThemeError> {
    let mut lines = text.lines();
    // A byte-order mark is three invisible bytes, and refusing a file over them
    // would be a rejection nobody could act on.
    let first = lines
        .next()
        .unwrap_or_default()
        .trim_start_matches('\u{feff}');
    if !first.trim().eq_ignore_ascii_case(HEADER) {
        return Err(ThemeError::NotATheme(path.to_path_buf()));
    }

    // Two passes over the body, because `base` decides what every absent token
    // falls back to and a file is free to name it after the colours. One pass
    // would make the fallback depend on the order somebody's editor left the
    // lines in.
    let body: Vec<(&str, &str)> = lines
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            Some((key.trim(), value.trim()))
        })
        .collect();

    let base = body
        .iter()
        .find(|(key, _)| *key == "base")
        .and_then(|(_, value)| base_from_id(value))
        .unwrap_or(ThemeKind::Graphite);

    let mut theme = CustomTheme::new(String::new(), base, Palette::of(base));
    let mut skipped = 0usize;
    for (key, value) in body {
        match key {
            "base" => {}
            // Through the same door the writer uses, so a hand-edited file
            // cannot carry a name the editor could not have produced.
            "name" => theme.name = clean_name(value),
            _ => match (Token::from_id(key), parse_hex(value)) {
                (Some(token), Some(colour)) => theme.palette.set_token(token, colour),
                // A token this build does not have, or a colour that will not
                // read. Either way the base's value stands and the count says
                // something was lost.
                _ => skipped += 1,
            },
        }
    }

    if theme.name.trim().is_empty() {
        // Through `clean_name` as well, and that is not belt and braces: a
        // filename is up to 255 characters of somebody else's choosing, so a
        // fallback taken verbatim would put a name on the card row that
        // [`MAX_NAME`] exists to say cannot get there.
        theme.name = path
            .file_stem()
            .map(|s| clean_name(&s.to_string_lossy()))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| UNTITLED.to_owned());
    }
    Ok(ThemeRead { theme, skipped })
}

// ---------------------------------------------------------------------------
// The library
// ---------------------------------------------------------------------------

/// Every theme the user has, one file each.
#[derive(Clone, Debug, Default)]
pub struct ThemeLibrary {
    dir: PathBuf,
    /// Sorted by name — see [`Self::sort`].
    themes: Vec<CustomTheme>,
    /// Stems of theme files in the directory that are **not** in `themes`.
    ///
    /// Kept for the reason `PaletteLibrary::occupied` is: a filename is an id
    /// here, so an id has to be free of every *file* rather than of every theme
    /// that happened to load. Without it, a file the library has just warned it
    /// could not read is a name [`Self::free_id`] hands straight out, and the
    /// next write renames over it — the user is told their theme is unreadable
    /// and then it is destroyed, in the same session.
    occupied: Vec<String>,
    warnings: Vec<String>,
}

impl ThemeLibrary {
    /// Directory name under the platform's user-data directory, beside
    /// `umber_core::preset::UserLibrary::DIR_NAME` and the palettes'.
    pub const DIR_NAME: &'static str = "themes";

    /// `%APPDATA%\Umber\data\themes` on Windows,
    /// `~/.local/share/umber/themes` on Linux,
    /// `~/Library/Application Support/Umber/themes` on macOS. `None` on a
    /// system with no home directory.
    ///
    /// The user's own data directory, which an update never touches — the
    /// reason `Library::collections` exists, and the reason a theme cannot live
    /// beside the two that are compiled in.
    pub fn default_dir() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "Umber")
            .map(|dirs| dirs.data_dir().join(Self::DIR_NAME))
    }

    pub fn load() -> Result<Self, ThemeError> {
        let dir = Self::default_dir().ok_or(ThemeError::NoDataDirectory)?;
        Ok(Self::load_from(dir))
    }

    /// Read every theme file in a directory.
    ///
    /// Never fails. A missing directory is an empty library — the state every
    /// user starts in — and a file that will not parse is a warning rather than
    /// a refusal: one bad file must not put the whole collection out of reach.
    pub fn load_from(dir: impl Into<PathBuf>) -> Self {
        let mut library = Self {
            dir: dir.into(),
            themes: Vec::new(),
            occupied: Vec::new(),
            warnings: Vec::new(),
        };
        let Ok(entries) = fs::read_dir(&library.dir) else {
            return library;
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION))
            })
            .collect();
        // The directory's own order is whatever the filesystem felt like, and a
        // library that reads back differently on two machines is one whose
        // first theme is a different theme on each.
        paths.sort();
        let mut over = false;
        for path in paths {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if library.themes.len() >= MAX_THEMES {
                // Every remaining file still owns its name, so the loop carries
                // on collecting stems rather than breaking. One warning, not
                // one per file.
                if !over {
                    over = true;
                    library.warnings.push(format!(
                        "{} holds more than {MAX_THEMES} themes; the rest were not read",
                        library.dir.display()
                    ));
                }
                library.occupied.push(stem);
                continue;
            }
            match Self::read_file(&path) {
                Ok(theme) => library.themes.push(theme),
                Err(e) => {
                    library.warnings.push(e.to_string());
                    library.occupied.push(stem);
                }
            }
        }
        library.sort();
        library
    }

    fn read_file(path: &Path) -> Result<CustomTheme, ThemeError> {
        let text = fs::read_to_string(path).map_err(|source| ThemeError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let mut theme = read_theme(&text, path)?.theme;
        theme.id = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(theme)
    }

    /// By name, case-folded, then by id — `PaletteLibrary::sort`'s rule and its
    /// argument.
    fn sort(&mut self) {
        self.themes.sort_by(|a, b| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        });
    }

    // There is deliberately no `dir()` accessor, unlike `PaletteLibrary`'s.
    // The one thing it would be for is a "Show the folder" control beside the
    // editor, and the Themes pane is the one page `docshot` photographs for the
    // README — a path on it would carry a contributor's home directory into a
    // committed picture, which is exactly what `prefs::set_config_path_label`
    // exists to stop. The warnings below already name the file when one will
    // not read, which is when somebody actually needs to find the folder.

    /// Anything that could not be read but did not stop the library loading.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn themes(&self) -> &[CustomTheme] {
        &self.themes
    }

    pub fn get(&self, id: &str) -> Option<&CustomTheme> {
        self.themes.iter().find(|t| t.id == id)
    }

    /// Whether another theme will fit. The New control reads this and is
    /// *disabled* when it is false, rather than being live and refusing.
    pub fn has_room(&self) -> bool {
        self.themes.len() < MAX_THEMES
    }

    /// Add a theme, or replace the one that already has its id, and write that
    /// one file.
    ///
    /// Returns the id, which the caller needs when the theme was built by
    /// [`CustomTheme::new`] and has none yet.
    pub fn save(&mut self, mut theme: CustomTheme) -> Result<String, ThemeError> {
        if theme.name.trim().is_empty() {
            theme.name = self.free_name(UNTITLED);
        }
        if theme.id.is_empty() {
            theme.id = self.free_id(&theme.name);
        }
        // Refused rather than written, and only for a theme that is *new*:
        // `load_from` stops reading at `MAX_THEMES`, so one written past it
        // would be in the list this session and gone the next. Saving an edit
        // to a theme already here is always allowed — it writes no new file.
        let id = theme.id.clone();
        let known = self.themes.iter().any(|t| t.id == id);
        if !known && !self.has_room() {
            return Err(ThemeError::Full { max: MAX_THEMES });
        }
        self.write(&theme)?;
        match self.themes.iter_mut().find(|t| t.id == id) {
            Some(existing) => *existing = theme,
            None => self.themes.push(theme),
        }
        self.sort();
        Ok(id)
    }

    /// Copy a palette into the library under a free name, and write it.
    ///
    /// The only way a theme is made. Starting from nothing would be a palette
    /// of transparent black, which is an interface nobody can see well enough
    /// to fix — so "New theme" means "a copy of the one in front of you", which
    /// is also what every application that has this feature means by it.
    ///
    /// Written out immediately, for the reason `PaletteLibrary::create` writes
    /// an empty palette: a theme that existed only in memory until its first
    /// edit would be one somebody could name, see in the list, and lose by
    /// closing the window.
    pub fn duplicate(
        &mut self,
        name: &str,
        base: ThemeKind,
        palette: Palette,
    ) -> Result<String, ThemeError> {
        let theme = CustomTheme::new(self.free_name(name), base, palette);
        self.save(theme)
    }

    /// Rename a theme. The id — and therefore the file — does not move.
    ///
    /// Deliberately: the id is what the preferences file holds, and renaming a
    /// file to match its title would orphan that reference in exchange for a
    /// tidier directory listing. `PaletteLibrary::rename`'s rule, and
    /// `BrushPreset::id`'s.
    ///
    /// A name something else already has is **numbered**, not taken —
    /// [`Self::free_name`]'s argument, which covers the built-ins as well: two
    /// cards both called Graphite are two cards you can only tell apart by
    /// which one has a Delete on it. It is a separate method from [`Self::save`]
    /// for exactly that reason. `save` may not free the name, because the name
    /// it is handed is usually the theme's own and freeing it would number a
    /// theme every time one of its colours changed.
    pub fn rename(&mut self, id: &str, name: &str) -> Result<(), ThemeError> {
        let Some(theme) = self.get(id) else {
            return Err(ThemeError::Unknown(id.to_owned()));
        };
        let mut renamed = theme.clone();
        // Against every *other* theme, so re-committing the name it already has
        // is not a rename to "Mine 2".
        let taken: Vec<String> = self
            .themes
            .iter()
            .filter(|t| t.id != id)
            .map(|t| t.name.clone())
            .collect();
        let built_in: Vec<&str> = ThemeKind::ALL.into_iter().map(ThemeKind::label).collect();
        renamed.name = umber_core::preset::unique_name(
            &clipped(name),
            UNTITLED,
            taken.iter().map(String::as_str).chain(built_in),
        );
        self.write(&renamed)?;
        if let Some(slot) = self.themes.iter_mut().find(|t| t.id == id) {
            *slot = renamed;
        }
        self.sort();
        Ok(())
    }

    /// Take a theme out of the library and delete its file.
    ///
    /// `false` means there was nothing with that id, which is not an error — a
    /// double click on Delete should not raise a dialog.
    pub fn remove(&mut self, id: &str) -> Result<bool, ThemeError> {
        let Some(index) = self.themes.iter().position(|t| t.id == id) else {
            return Ok(false);
        };
        let path = self.path_of(id);
        match fs::remove_file(&path) {
            Ok(()) => {}
            // Already gone is the outcome that was wanted.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(ThemeError::Io { path, source }),
        }
        self.themes.remove(index);
        Ok(true)
    }

    /// Read a theme file from anywhere and put a copy in the library.
    ///
    /// Returns the new id and how many lines the file lost on the way in — see
    /// [`ThemeRead::skipped`].
    pub fn import(&mut self, path: &Path) -> Result<(String, usize), ThemeError> {
        let text = fs::read_to_string(path).map_err(|source| ThemeError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let read = read_theme(&text, path)?;
        let mut theme = read.theme;
        // A name already in the library gets a number, rather than the import
        // quietly replacing a theme somebody built.
        theme.name = self.free_name(&theme.name);
        theme.id = String::new();
        let id = self.save(theme)?;
        Ok((id, read.skipped))
    }

    /// Write one theme out to a path of the caller's choosing.
    pub fn export(&self, id: &str, path: &Path) -> Result<(), ThemeError> {
        let theme = self
            .get(id)
            .ok_or_else(|| ThemeError::Unknown(id.to_owned()))?;
        write_atomically(path, &theme.to_text())
    }

    /// The file one id names.
    ///
    /// `slug` is applied here and not only where an id is *minted*, because
    /// [`CustomTheme::id`] is a public field and this is the one place it
    /// becomes a path. "The callers only pass ids this module made" is not good
    /// enough — it is the standard `autosave::Reaper` is held to, and the
    /// failure it prevents is a name with a separator in it writing outside the
    /// library.
    fn path_of(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{}.{EXTENSION}", slug(id)))
    }

    fn write(&self, theme: &CustomTheme) -> Result<(), ThemeError> {
        if let Err(source) = fs::create_dir_all(&self.dir) {
            return Err(ThemeError::Io {
                path: self.dir.clone(),
                source,
            });
        }
        write_atomically(&self.path_of(&theme.id), &theme.to_text())
    }

    /// A display name nothing else in the library is called, and nothing built
    /// in is called either — a second "Graphite" in the card row would be two
    /// cards somebody has to tell apart by which one has a delete on it.
    fn free_name(&self, desired: &str) -> String {
        let built_in: Vec<&str> = ThemeKind::ALL.into_iter().map(ThemeKind::label).collect();
        umber_core::preset::unique_name(
            &clipped(desired),
            UNTITLED,
            self.themes.iter().map(|t| t.name.as_str()).chain(built_in),
        )
    }

    /// A filename stem no file in the directory already occupies.
    ///
    /// Judged against **every theme file that was seen**, not against the
    /// themes that loaded — see [`Self::occupied`].
    fn free_id(&self, name: &str) -> String {
        let base = slug(name);
        let taken: Vec<&str> = self
            .themes
            .iter()
            .map(|t| t.id.as_str())
            .chain(self.occupied.iter().map(String::as_str))
            .collect();
        if !taken.contains(&base.as_str()) {
            return base;
        }
        // From 2, the way `unique_name` counts. Not `unique_name` itself: that
        // compares case-folded, which is right for something a person reads and
        // wrong for a filename — two ids differing only in case are two files
        // on Linux and one on Windows, and the tie has to be broken here rather
        // than by whichever platform the library is opened on.
        let mut n = 2u32;
        loop {
            let candidate = format!("{base}-{n}");
            if !taken.contains(&candidate.as_str()) {
                return candidate;
            }
            n += 1;
        }
    }
}

/// A filename stem from a display name: lower-case ASCII letters and digits,
/// anything else a single hyphen.
///
/// `umber_core::palette`'s rule, restated here rather than shared because that
/// one is private to a module in another crate and the argument for it is the
/// same: a theme whose file will not open on a machine it was copied to is a
/// worse outcome than one whose filename lost its accents, and the name
/// somebody reads comes out of the file's `name` line rather than out of the
/// path.
fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    // Truncated at a length every filesystem accepts with room for the
    // disambiguating suffix and the extension.
    let cut = trimmed
        .char_indices()
        .nth(48)
        .map_or(trimmed.len(), |(i, _)| i);
    match &trimmed[..cut] {
        "" => "theme".to_owned(),
        s => s.to_owned(),
    }
}

/// Write a file beside itself and rename over it.
///
/// The same trick `PaletteLibrary` and `UserLibrary` use, and for the same
/// reason: a write interrupted by a full disk would otherwise leave a truncated
/// file where a theme used to be.
fn write_atomically(path: &Path, text: &str) -> Result<(), ThemeError> {
    let mut temporary = path.to_path_buf().into_os_string();
    temporary.push(".saving");
    let temporary = PathBuf::from(temporary);
    fs::write(&temporary, text).map_err(|source| ThemeError::Io {
        path: temporary.clone(),
        source,
    })?;
    if let Err(source) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(ThemeError::Io {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Resolving the preference
// ---------------------------------------------------------------------------

/// The palette a stored preference names.
///
/// One door, shared by the application and by the crash reporter — which is a
/// second process that has only the preferences file to go on, so without this
/// a crash box would come up in Graphite for somebody who has never seen it.
/// The library is only read when a custom theme is actually named, so the
/// ordinary path touches no disk.
pub fn resolve(theme: ThemeKind, accent: crate::theme::Accent, custom: Option<&str>) -> Palette {
    let Some(id) = custom else {
        return Palette::with_accent(theme, accent);
    };
    match ThemeLibrary::load() {
        Ok(library) => match library.get(id) {
            Some(found) => found.palette,
            // The theme was deleted, or the data directory moved. Falling back
            // to the built-in the preference also names is the one answer that
            // cannot leave somebody with an interface they cannot read.
            None => Palette::with_accent(theme, accent),
        },
        Err(_) => Palette::with_accent(theme, accent),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn here() -> PathBuf {
        PathBuf::from("test.umbertheme")
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("umber-theme-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn same(a: &Palette, b: &Palette) -> bool {
        Token::ALL.into_iter().all(|t| a.token(t) == b.token(t))
    }

    /// The one thing this module exists to guarantee: a theme survives being
    /// written and read back, byte for byte in every token. It holds because a
    /// token is stored in the form it is written in — the argument
    /// `umber_core::palette` makes for eight-bit sRGB, applied here.
    #[test]
    fn a_theme_survives_being_written_and_read() {
        let mut palette = Palette::of(ThemeKind::Paper);
        // Every token moved, so a field the writer forgot cannot pass by
        // happening to equal the base's value.
        for (n, token) in Token::ALL.into_iter().enumerate() {
            palette.set_token(token, Color32::from_rgb(n as u8, 255 - n as u8, 0x5A));
        }
        let theme = CustomTheme::new("Midnight oil", ThemeKind::Paper, palette);

        let read = read_theme(&theme.to_text(), &here()).expect("written by us");
        assert_eq!(read.skipped, 0);
        assert_eq!(read.theme.name, "Midnight oil");
        assert_eq!(read.theme.base, ThemeKind::Paper);
        assert!(
            same(&read.theme.palette, &palette),
            "a token did not survive the round trip"
        );
    }

    /// Every token has to be writable *and* readable under its own id. A field
    /// missing from either `match` in `Palette::token`/`set_token` would show
    /// up as one token reading as its neighbour, which is invisible in a
    /// round trip that moved them all by the same rule.
    #[test]
    fn every_token_is_stored_under_a_name_of_its_own() {
        let mut ids: Vec<&str> = Token::ALL.into_iter().map(Token::id).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "two tokens share a stored name");

        for token in Token::ALL {
            let mut palette = Palette::of(ThemeKind::Graphite);
            palette.set_token(token, Color32::from_rgb(1, 2, 3));
            let text = CustomTheme::new("t", ThemeKind::Graphite, palette).to_text();
            let back = read_theme(&text, &here()).expect("written by us").theme;
            assert_eq!(
                back.palette.token(token),
                Color32::from_rgb(1, 2, 3),
                "{} did not come back",
                token.id()
            );
            // And nothing else moved with it.
            for other in Token::ALL.into_iter().filter(|t| *t != token) {
                assert_eq!(
                    back.palette.token(other),
                    Palette::of(ThemeKind::Graphite).token(other),
                    "setting {} also moved {}",
                    token.id(),
                    other.id()
                );
            }
        }
    }

    /// Every token needs a heading, or the editor draws a row nobody can find.
    #[test]
    fn every_token_is_under_exactly_one_heading() {
        let mut seen = Vec::new();
        for group in crate::theme::TokenGroup::ALL {
            seen.extend(group.tokens());
        }
        assert_eq!(
            seen.len(),
            Token::ALL.len(),
            "a token is under no heading, or under two"
        );
        for token in Token::ALL {
            assert!(seen.contains(&token), "{} has no heading", token.id());
        }
    }

    /// A file written by an older or newer Umber. What it does not carry takes
    /// the *base* theme's value — not Graphite's, or a light theme would gain a
    /// near-black surface for every token added since it was written.
    #[test]
    fn a_token_a_file_does_not_carry_comes_from_the_base_theme() {
        let text = concat!(
            "Umber theme\n",
            "name = Half a theme\n",
            "base = paper\n",
            "accent = #123456\n",
        );
        let read = read_theme(text, &here()).expect("a header and two lines");
        assert_eq!(
            read.theme.palette.accent,
            Color32::from_rgb(0x12, 0x34, 0x56)
        );
        assert_eq!(read.theme.palette.window, Palette::paper().window);
        assert_eq!(read.theme.palette.text, Palette::paper().text);
        assert_eq!(read.skipped, 0, "an absent token is not a loss");
    }

    /// `base` decides what every absent token falls back to, so it must not
    /// depend on where in the file it happens to sit.
    #[test]
    fn base_is_read_before_the_colours_whatever_order_the_file_is_in() {
        let text = concat!("Umber theme\n", "accent = #123456\n", "base = paper\n");
        let read = read_theme(text, &here()).expect("a header and two lines");
        assert_eq!(read.theme.base, ThemeKind::Paper);
        assert_eq!(read.theme.palette.window, Palette::paper().window);
    }

    /// A line that will not read costs that one colour and is counted — the
    /// tolerance `prefs` gives, plus `docimport`'s rule that a loss is named.
    #[test]
    fn a_line_that_will_not_read_costs_only_itself_and_is_counted() {
        let text = concat!(
            "Umber theme\n",
            "base = graphite\n",
            "accent = not a colour\n",
            "window = #101112\n",
            "onion_skin = #FFFFFF\n",
            "\u{0}\u{0} binary rubbish from a truncated write\n",
        );
        let read = read_theme(text, &here()).expect("the header is intact");
        assert_eq!(
            read.theme.palette.window,
            Color32::from_rgb(0x10, 0x11, 0x12)
        );
        assert_eq!(
            read.theme.palette.accent,
            Palette::graphite().accent,
            "a colour that would not read must leave the base's in place"
        );
        assert_eq!(read.skipped, 2, "the bad colour and the unknown token");
    }

    /// A file that is not a theme is refused with a sentence rather than read
    /// as a theme of entirely default colours — which would be a "successful"
    /// import of Graphite under somebody else's filename.
    #[test]
    fn a_file_that_is_not_a_theme_is_refused() {
        assert!(read_theme("GIMP Palette\nName: Ochres\n", &here()).is_err());
        assert!(read_theme("", &here()).is_err());
        // A byte-order mark is three invisible bytes and must not be a refusal.
        assert!(read_theme("\u{feff}Umber theme\n", &here()).is_ok());
    }

    #[test]
    fn a_hex_is_read_in_every_form_a_person_types_and_no_other() {
        assert_eq!(
            parse_hex("#C08A4E"),
            Some(Color32::from_rgb(0xC0, 0x8A, 0x4E))
        );
        assert_eq!(
            parse_hex("c08a4e"),
            Some(Color32::from_rgb(0xC0, 0x8A, 0x4E))
        );
        assert_eq!(parse_hex("  #FFF  "), Some(Color32::WHITE));
        assert_eq!(parse_hex("#012"), Some(Color32::from_rgb(0x00, 0x11, 0x22)));
        for bad in [
            "",
            "#",
            "#12",
            "#12345",
            "#12345678",
            "rebeccapurple",
            "#GGGGGG",
        ] {
            assert_eq!(parse_hex(bad), None, "{bad} was read as a colour");
        }
        // The pair has to be an exact inverse, or a theme moves a level every
        // time it is saved and reopened — `docimport::srgb`'s rule.
        for n in 0..=255u8 {
            let c = Color32::from_rgb(n, 255 - n, n / 2);
            assert_eq!(parse_hex(&hex(c)), Some(c));
        }
    }

    /// A name with a newline in it would produce a file whose next line is read
    /// as a token, and one that is empty would produce a card with no label.
    #[test]
    fn a_name_cannot_break_the_file_it_is_written_into() {
        let theme = CustomTheme::new(
            "Two\nlines",
            ThemeKind::Graphite,
            Palette::of(ThemeKind::Graphite),
        );
        let back = read_theme(&theme.to_text(), &here())
            .expect("written by us")
            .theme;
        assert_eq!(back.name, "Two lines");

        let blank = CustomTheme::new("   ", ThemeKind::Graphite, Palette::of(ThemeKind::Graphite));
        let back = read_theme(&blank.to_text(), &here())
            .expect("written by us")
            .theme;
        assert_eq!(back.name, UNTITLED);
    }

    #[test]
    fn a_theme_survives_the_library_being_written_and_read_back() {
        let dir = temp_dir("roundtrip");
        let mut library = ThemeLibrary::load_from(&dir);
        assert!(library.themes().is_empty());

        let mut palette = Palette::of(ThemeKind::Graphite);
        palette.set_token(Token::Accent, Color32::from_rgb(0x11, 0x22, 0x33));
        let id = library
            .duplicate("Midnight", ThemeKind::Graphite, palette)
            .expect("a fresh directory");

        let reopened = ThemeLibrary::load_from(&dir);
        let theme = reopened.get(&id).expect("written to disk");
        assert_eq!(theme.name, "Midnight");
        assert_eq!(theme.palette.accent, Color32::from_rgb(0x11, 0x22, 0x33));
        let _ = fs::remove_dir_all(&dir);
    }

    /// A theme is duplicated from whatever is in front of the user, so two
    /// copies of Graphite must not both be called Graphite — and neither may be
    /// called Graphite at all, since a built-in already is and the two cards
    /// would be indistinguishable.
    #[test]
    fn a_duplicate_never_takes_a_name_something_else_already_has() {
        let dir = temp_dir("names");
        let mut library = ThemeLibrary::load_from(&dir);
        let graphite = Palette::of(ThemeKind::Graphite);
        let first = library
            .duplicate("Graphite", ThemeKind::Graphite, graphite)
            .expect("room");
        let second = library
            .duplicate("Graphite", ThemeKind::Graphite, graphite)
            .expect("room");
        assert_ne!(first, second);
        let names: Vec<&str> = library.themes().iter().map(|t| t.name.as_str()).collect();
        assert!(
            !names.contains(&"Graphite"),
            "a custom theme took a built-in's name: {names:?}"
        );
        assert_eq!(
            names.len(),
            2,
            "the two must not be the same name: {names:?}"
        );
        assert_ne!(names[0], names[1]);
        let _ = fs::remove_dir_all(&dir);
    }

    /// The guard `PaletteLibrary` learned the hard way: a file the library has
    /// just said it could not read still owns its name, or the next write
    /// renames over it and destroys the thing the user was warned about.
    #[test]
    fn a_file_that_would_not_read_is_never_written_over() {
        let dir = temp_dir("occupied");
        fs::create_dir_all(&dir).expect("temp");
        let squatter = dir.join(format!("midnight.{EXTENSION}"));
        fs::write(&squatter, "this is not a theme at all\n").expect("temp");

        let mut library = ThemeLibrary::load_from(&dir);
        assert!(library.themes().is_empty());
        assert_eq!(library.warnings().len(), 1);

        let id = library
            .duplicate(
                "Midnight",
                ThemeKind::Graphite,
                Palette::of(ThemeKind::Graphite),
            )
            .expect("room");
        assert_ne!(id, "midnight", "the unreadable file's name was handed out");
        assert_eq!(
            fs::read_to_string(&squatter).expect("still there"),
            "this is not a theme at all\n"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// An id is a public field and this is the one place it becomes a path, so
    /// a name with a separator in it must not write outside the library — the
    /// standard `autosave::Reaper` is held to.
    #[test]
    fn a_name_with_a_separator_in_it_cannot_write_outside_the_library() {
        let dir = temp_dir("escape");
        let mut library = ThemeLibrary::load_from(&dir);
        let id = library
            .duplicate(
                "../../ochres / greys",
                ThemeKind::Paper,
                Palette::of(ThemeKind::Paper),
            )
            .expect("room");
        assert!(!id.contains(['/', '\\', ':', '.']), "{id} is not a stem");
        let written: Vec<PathBuf> = fs::read_dir(&dir)
            .expect("the directory was made")
            .flatten()
            .map(|e| e.path())
            .collect();
        assert_eq!(written.len(), 1, "{written:?}");
        assert_eq!(written[0], dir.join(format!("{id}.{EXTENSION}")));
        let _ = fs::remove_dir_all(&dir);
    }

    /// A rename is the third way a name is chosen, after `duplicate` and
    /// `import`, and it has to be held to the same rule as those two: two cards
    /// called Graphite are two cards you can only tell apart by which one has a
    /// Delete on it. It must also *not* number a theme for keeping the name it
    /// already has, which is what going through `save` would have done.
    #[test]
    fn a_rename_cannot_take_a_name_something_else_already_has() {
        let dir = temp_dir("rename");
        let mut library = ThemeLibrary::load_from(&dir);
        let graphite = Palette::of(ThemeKind::Graphite);
        let first = library
            .duplicate("Dusk", ThemeKind::Graphite, graphite)
            .unwrap();
        let second = library
            .duplicate("Dawn", ThemeKind::Graphite, graphite)
            .unwrap();

        library
            .rename(&second, "Dusk")
            .expect("it is in the library");
        assert_ne!(
            library.get(&second).unwrap().name,
            "Dusk",
            "two themes took one name"
        );
        assert_eq!(
            library.get(&first).unwrap().name,
            "Dusk",
            "and the first kept it"
        );

        library.rename(&first, "Graphite").expect("in the library");
        assert_ne!(
            library.get(&first).unwrap().name,
            "Graphite",
            "a custom theme took a built-in's name"
        );

        // Re-committing the name it already holds is not a rename to "X 2" —
        // which is what happens on every blur of the name field, so it has to
        // be a no-op.
        let held = library.get(&first).unwrap().name.clone();
        library.rename(&first, &held).expect("in the library");
        assert_eq!(library.get(&first).unwrap().name, held);

        // And it reaches the file, not only the list.
        assert_eq!(
            ThemeLibrary::load_from(&dir)
                .get(&first)
                .map(|t| t.name.clone()),
            Some(held)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// A name is the one free-text field in a theme file and it ends up on a
    /// 150-point card, so it is bounded on the way in as well as on the way
    /// out — and the number a duplicate takes has to survive that bound, or two
    /// themes come back on one name.
    #[test]
    fn a_name_is_bounded_in_both_directions_and_leaves_room_for_its_number() {
        let long: String = "wide".repeat(200);
        let theme = CustomTheme::new(
            long.clone(),
            ThemeKind::Graphite,
            Palette::of(ThemeKind::Graphite),
        );
        let back = read_theme(&theme.to_text(), &here())
            .expect("written by us")
            .theme;
        assert!(back.name.chars().count() <= MAX_NAME, "{}", back.name.len());

        // Straight out of a hand-edited file, not through the writer.
        let text = format!("Umber theme\nbase = graphite\nname = {long}\n");
        let read = read_theme(&text, &here()).expect("a header and a name");
        assert!(read.theme.name.chars().count() <= MAX_NAME);

        // And the *filename* fallback, which is the path a file with a blank
        // name line takes — 255 characters of somebody else's choosing, and the
        // one route into the card row that was not going through the cap.
        let stem = PathBuf::from(format!("{long}.{EXTENSION}"));
        let read = read_theme("Umber theme\nname =\n", &stem).expect("a header");
        assert!(
            read.theme.name.chars().count() <= MAX_NAME,
            "the filename fallback is {} characters",
            read.theme.name.chars().count()
        );

        let dir = temp_dir("long-names");
        let mut library = ThemeLibrary::load_from(&dir);
        let graphite = Palette::of(ThemeKind::Graphite);
        let a = library
            .duplicate(&long, ThemeKind::Graphite, graphite)
            .unwrap();
        let b = library
            .duplicate(&long, ThemeKind::Graphite, graphite)
            .unwrap();
        let (a, b) = (
            library.get(&a).unwrap().name.clone(),
            library.get(&b).unwrap().name.clone(),
        );
        assert_ne!(a, b, "the number was the part the cap cut off");
        // And it is still two names after a round trip through the file.
        let reopened = ThemeLibrary::load_from(&dir);
        let mut names: Vec<&str> = reopened.themes().iter().map(|t| t.name.as_str()).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "{names:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn deleting_a_theme_takes_its_file_with_it() {
        let dir = temp_dir("delete");
        let mut library = ThemeLibrary::load_from(&dir);
        let id = library
            .duplicate(
                "Gone",
                ThemeKind::Graphite,
                Palette::of(ThemeKind::Graphite),
            )
            .expect("room");
        assert!(library.remove(&id).expect("it is there"));
        assert!(library.get(&id).is_none());
        assert!(!dir.join(format!("{id}.{EXTENSION}")).exists());
        // A second click must not raise a dialog.
        assert!(!library.remove(&id).expect("already gone is not an error"));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Import and export go through the one encoder and the one decoder, so a
    /// theme taken out and brought back has to be the same theme — under a
    /// different name, because the one it had is now taken.
    #[test]
    fn a_theme_exported_and_imported_again_is_the_same_theme() {
        let dir = temp_dir("interchange");
        let mut library = ThemeLibrary::load_from(&dir);
        let mut palette = Palette::of(ThemeKind::Paper);
        palette.set_token(Token::Knob, Color32::from_rgb(9, 8, 7));
        let id = library
            .duplicate("Travelled", ThemeKind::Paper, palette)
            .expect("room");

        let out = temp_dir("interchange-out");
        fs::create_dir_all(&out).expect("temp");
        let file = out.join(format!("shared.{EXTENSION}"));
        library.export(&id, &file).expect("a writable directory");

        let (back, skipped) = library.import(&file).expect("we just wrote it");
        assert_eq!(skipped, 0);
        let brought = library.get(&back).expect("imported");
        assert!(same(&brought.palette, &palette));
        assert_eq!(brought.base, ThemeKind::Paper);
        assert_ne!(brought.name, "Travelled", "an import must not replace");
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&out);
    }

    /// With no custom theme named, nothing here touches the disk and the answer
    /// is exactly what it was before this module existed.
    #[test]
    fn no_custom_theme_named_is_the_built_in_palette_exactly() {
        for kind in ThemeKind::ALL {
            for accent in crate::theme::Accent::ALL {
                let expected = Palette::with_accent(kind, accent);
                let got = resolve(kind, accent, None);
                assert!(same(&got, &expected), "{kind:?}/{accent:?}");
            }
        }
    }
}
