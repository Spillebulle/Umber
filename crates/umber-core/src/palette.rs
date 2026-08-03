//! Palettes: a named row of colours the artist keeps, and the library of them
//! on disk.
//!
//! Everything here is plain data — no GPU types, no file dialogs, no UI — for
//! the reason [`crate::preset`] is: what a palette *is*, what a library is,
//! how one is named and where it lives are rules, and a rule is testable
//! without a window.
//!
//! # A swatch is eight-bit sRGB, not a `Color`
//!
//! [`Color`] is linear f32, which is what the engine paints with. A palette is
//! not painted with; it is *stored*, shared, and read by other applications,
//! and every one of those holds eight-bit sRGB. Keeping a swatch in the form it
//! is stored in makes the round trip exact by construction: what the artist
//! saved is byte for byte what comes back, and there is no drift to accumulate
//! across a save and a reload. A linear `Color` would be quantised on the way
//! out and dequantised on the way in, and the colour you clicked would not be
//! the colour you got.
//!
//! This is the same argument [`crate::clipboard`] makes for holding straight
//! alpha sRGB rather than layer bytes, and the conversion at each boundary goes
//! through [`Color::from_srgb_u8`] and [`Color::to_srgb_u8`] — the exact
//! inverse pair `srgb_roundtrip_is_stable` already pins — rather than through a
//! second `powf` written here. (`docimport::srgb`'s pair is the *premultiplied*
//! one, and at the opaque alpha a swatch always has it is the identity, so the
//! two agree.)
//!
//! # Why the library is a directory of `.gpl` files
//!
//! [`crate::preset::UserLibrary`] is a directory because a bitmap tip does not
//! go in a text file. A palette holds no bitmaps, so that argument does not
//! carry over and the question had to be asked again. It is still a directory,
//! and for a stronger reason: **the interchange format is the storage format**.
//!
//! GIMP's `.gpl` is the universal palette format. It is a dozen lines of plain
//! text, and GIMP, Krita, Inkscape and Aseprite all read it — the four named
//! because those four are checkable, not because the list ends there, and
//! *read* rather than "read and write" because Inkscape's palettes **are**
//! `.gpl` files and it has no export for them. Storing the library as one
//! `.gpl` per palette means:
//!
//! - **There is one decoder and one encoder.** Import is reading a file into
//!   the directory and export is copying one out; neither is a second parser to
//!   keep in step with the first. That is the rule `docformat` states as "there
//!   must never be a second ORA reader", applied where the format Umber reads
//!   and the format Umber writes are the same one.
//! - **A write touches only what changed.** Adding a swatch rewrites one small
//!   file, not an index holding every palette — and the library is written on
//!   *every* edit, so that is the common case.
//! - **The files are ordinary files.** A palette is something people swap.
//!   Being able to hand somebody the file, or point GIMP straight at the
//!   directory, is worth more than tidiness.
//!
//! The cost is that there is nowhere for anything Umber-specific to live. So
//! far nothing needs to: a palette is a name, a column count and a list of
//! named colours, and `.gpl` carries all four. If something ever does, it goes
//! in a `#` comment line — which every other reader already ignores, exactly as
//! every other ORA reader ignores the `umber-` attributes.
//!
//! # Nothing is shipped, deliberately
//!
//! There is no built-in half of this library and therefore no merge. A palette
//! Umber shipped would have to be somebody's authored set, correctly
//! transcribed and correctly attributed, and a mis-transcribed palette under an
//! author's name is the failure `examples/build-brush-library.rs` refuses for
//! brushes. An empty library with a New button and an Import button is honest,
//! and a `.gpl` is one click away from every other application the artist
//! already has.
//!
//! If a shipped half is ever added, the rule it has to follow is the one
//! [`crate::preset::Library::collections`] exists for and is worth writing down
//! before rather than after: a shipped palette is compiled into the binary and
//! replaced wholesale by every update, so **anything the user decides about
//! one — a rename, a reordering, a swatch added — cannot be written where the
//! palette is.** It would have to be a copy in the user's own directory, keyed
//! by the shipped palette's stable id. A choice written into the shipped half
//! survives until the next release and then vanishes silently, months later.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::color::Color;
use crate::preset::unique_name;

/// The extension every palette file carries, and the only one the library
/// reads.
pub const GPL_EXTENSION: &str = "gpl";

/// The line every `.gpl` starts with.
const GPL_HEADER: &str = "GIMP Palette";

/// The most colours one palette may hold.
///
/// A bound rather than a design value. A palette is something a person builds
/// by hand, and the largest anybody ships is a few hundred; four thousand is
/// far past that and is still small enough to draw and to hold. What it stops
/// is a `.gpl` produced by quantising a photograph — or a file that is not a
/// palette at all — being read into memory unbounded.
pub const MAX_SWATCHES: usize = 4096;

/// The most palettes one library directory may hold.
///
/// The directory is read whole at startup, so this is what stops a folder
/// somebody pointed at a colour-scheme dump costing a second of launch.
pub const MAX_PALETTES: usize = 512;

/// What a palette falls back to being called.
pub const UNTITLED: &str = "Untitled palette";

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum PaletteError {
    /// The file does not begin with `GIMP Palette`.
    NotAPalette(PathBuf),
    TooManySwatches {
        path: PathBuf,
        found: usize,
        max: usize,
    },
    /// The library already holds as many palettes as it will read back.
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

impl fmt::Display for PaletteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAPalette(path) => write!(
                f,
                "{} is not a GIMP palette — the first line has to be “{GPL_HEADER}”",
                path.display()
            ),
            Self::TooManySwatches { path, found, max } => write!(
                f,
                "{} holds {found} colours, and Umber reads at most {max} in one palette",
                path.display()
            ),
            Self::Full { max } => write!(
                f,
                "the library already holds {max} palettes, which is as many as Umber reads back"
            ),
            Self::Unknown(id) => write!(f, "there is no palette called “{id}” in the library"),
            Self::NoDataDirectory => {
                write!(
                    f,
                    "this system has no user data directory to keep palettes in"
                )
            }
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
        }
    }
}

impl std::error::Error for PaletteError {}

// ---------------------------------------------------------------------------
// A palette
// ---------------------------------------------------------------------------

/// One colour in a palette, in the form it is stored and shared in.
///
/// The name may be empty. `.gpl` allows it, and a colour picked off the canvas
/// has no name anybody chose — inventing "Untitled" for it would put a word in
/// every cell of a grid that is meant to be colours.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Swatch {
    /// sRGB, opaque. See the module docs for why this is not a [`Color`].
    pub rgb: [u8; 3],
    pub name: String,
}

impl Swatch {
    pub fn new(rgb: [u8; 3]) -> Self {
        Self {
            rgb,
            name: String::new(),
        }
    }

    /// The engine's colour as a swatch. Alpha is dropped: a palette names a
    /// colour, and how much of it goes down is the brush's opacity.
    pub fn of(colour: Color) -> Self {
        let [r, g, b, _] = colour.to_srgb_u8();
        Self::new([r, g, b])
    }

    /// Back to the engine's linear colour, fully opaque.
    pub fn colour(&self) -> Color {
        Color::from_srgb_u8(self.rgb[0], self.rgb[1], self.rgb[2], 255)
    }

    /// `#RRGGBB`, which is what the panel shows when a swatch has no name.
    pub fn hex(&self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.rgb[0], self.rgb[1], self.rgb[2])
    }
}

/// A named row of colours.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Palette {
    /// Stable and opaque, and in a [`PaletteLibrary`] it is the file's stem —
    /// so the filesystem is what keeps it unique and there is no second table
    /// to keep in step. Never derived from the current name at read time:
    /// renaming a palette must not orphan the selection pointing at it.
    pub id: String,
    pub name: String,
    /// How many swatches a row holds, or zero for "the panel decides".
    ///
    /// `.gpl`'s own `Columns:` header, carried across rather than discarded,
    /// because a palette laid out in fours by whoever made it says something
    /// about how it is meant to be read.
    pub columns: u32,
    pub swatches: Vec<Swatch>,
}

impl Palette {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: String::new(),
            name: name.into(),
            columns: 0,
            swatches: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.swatches.len()
    }

    pub fn is_empty(&self) -> bool {
        self.swatches.is_empty()
    }

    /// Whether another colour will fit. The panel's Add control reads this and
    /// is *disabled* when it is false, rather than being live and refusing.
    pub fn has_room(&self) -> bool {
        self.swatches.len() < MAX_SWATCHES
    }

    /// Append a colour. Returns false when the palette is full, which is the
    /// only reason this can fail.
    ///
    /// A colour already in the palette is added again rather than refused. A
    /// painter who clicks Add twice has made a duplicate, which they can see
    /// and remove; a control that silently did nothing would be one they had to
    /// work out.
    pub fn add(&mut self, swatch: Swatch) -> bool {
        if !self.has_room() {
            return false;
        }
        self.swatches.push(swatch);
        true
    }

    /// Take one out. Out of range is `None` rather than a panic: the index came
    /// from a grid drawn against last frame's palette.
    pub fn remove(&mut self, index: usize) -> Option<Swatch> {
        (index < self.swatches.len()).then(|| self.swatches.remove(index))
    }

    /// Serialise to `.gpl`.
    ///
    /// Laid out the way GIMP lays it out — three right-aligned decimal
    /// components, then a tab and the name — because the file is meant to be
    /// opened by other applications and by people, and matching what they
    /// already write costs nothing.
    pub fn to_gpl(&self) -> String {
        let mut out = String::from(GPL_HEADER);
        out.push('\n');
        // A `Name:` header is what every reader shows, and a palette with none
        // shows as its filename. The name is always written even when it is the
        // fallback, so a file that leaves Umber never relies on its own stem.
        out.push_str(&format!("Name: {}\n", one_line(&self.name)));
        if self.columns > 0 {
            out.push_str(&format!("Columns: {}\n", self.columns));
        }
        out.push_str("#\n");
        for swatch in &self.swatches {
            out.push_str(&format!(
                "{:3} {:3} {:3}",
                swatch.rgb[0], swatch.rgb[1], swatch.rgb[2]
            ));
            if !swatch.name.trim().is_empty() {
                out.push('\t');
                out.push_str(&one_line(&swatch.name));
            }
            out.push('\n');
        }
        out
    }
}

/// A name with anything that would break the line format taken out.
///
/// `.gpl` is line-oriented with no escaping, so a newline in a name would
/// produce a file whose next line is read as a colour — or, worse, silently
/// truncate the palette there. Tabs go too, because a tab is what separates a
/// colour from its name.
fn one_line(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_control() || c == '\t' { ' ' } else { c })
        .collect();
    match cleaned.trim() {
        "" => UNTITLED.to_owned(),
        trimmed => trimmed.to_owned(),
    }
}

/// What came of reading a `.gpl`.
///
/// The skipped count is here rather than swallowed because a file that lost
/// lines on the way in is exactly what [`crate::docimport`]'s rule covers: an
/// import that loses something must say so, since subtly wrong content is worse
/// than a refusal. Every reader in the wild skips a line it cannot parse — GIMP
/// included — so refusing the whole file over one stray line would be worse
/// still; counting it is the middle answer.
#[derive(Clone, Debug)]
pub struct GplRead {
    pub palette: Palette,
    /// Lines that were neither a header, a comment, nor three numbers.
    pub skipped: usize,
}

/// Parse a `.gpl`.
///
/// `path` is only ever used to phrase the error, so a caller with the text in
/// hand and no file can pass any name.
pub fn read_gpl(text: &str, path: &Path) -> Result<GplRead, PaletteError> {
    let mut lines = text.lines();
    // Some writers put a byte-order mark on the first line, and refusing a file
    // over three invisible bytes would be a rejection nobody could act on.
    let first = lines
        .next()
        .unwrap_or_default()
        .trim_start_matches('\u{feff}');
    if !first.trim().eq_ignore_ascii_case(GPL_HEADER) {
        return Err(PaletteError::NotAPalette(path.to_path_buf()));
    }

    let mut palette = Palette::new(String::new());
    let mut skipped = 0usize;
    for line in lines {
        let line = line.trim_end_matches(['\r', '\n']);
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // Headers, which only appear before the colours in practice but are
        // accepted anywhere: a file where one strayed lower should not lose its
        // name over it.
        if let Some(rest) = strip_header(trimmed, "Name:") {
            palette.name = rest.to_owned();
            continue;
        }
        if let Some(rest) = strip_header(trimmed, "Columns:") {
            palette.columns = rest.trim().parse().unwrap_or(0);
            continue;
        }
        match parse_entry(trimmed) {
            Some(swatch) => {
                if palette.swatches.len() >= MAX_SWATCHES {
                    return Err(PaletteError::TooManySwatches {
                        path: path.to_path_buf(),
                        found: palette.swatches.len() + 1,
                        max: MAX_SWATCHES,
                    });
                }
                palette.swatches.push(swatch);
            }
            None => skipped += 1,
        }
    }

    // A palette with no colours in it is **not** refused, and that is not an
    // oversight. [`PaletteLibrary::create`] writes one out empty — a palette
    // exists from the moment it is named, so that closing the window does not
    // lose it — and a reader that turned round and refused its own output would
    // make every new palette vanish on the next launch, with a dialog saying it
    // had no colours in it. There is nothing to warn about either: the panel
    // shows an empty grid and says so.
    if palette.name.trim().is_empty() {
        // The filename is what every other reader falls back to, so fall back
        // to the same thing rather than to the word "Untitled".
        palette.name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| UNTITLED.to_owned());
    }
    Ok(GplRead { palette, skipped })
}

fn strip_header<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let rest = line.get(..key.len())?;
    rest.eq_ignore_ascii_case(key)
        .then(|| line[key.len()..].trim())
}

/// One colour line: three numbers `0..=255`, then anything else as its name.
///
/// Components out of range are what tells a colour line from a line of prose
/// that happens to start with digits, so they are a refusal rather than a
/// clamp — a clamp would read "1999 Vintage Reds" as a colour.
fn parse_entry(line: &str) -> Option<Swatch> {
    let mut rest = line;
    let mut rgb = [0u8; 3];
    for slot in &mut rgb {
        rest = rest.trim_start();
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if end == 0 {
            return None;
        }
        *slot = rest[..end].parse::<u32>().ok()?.try_into().ok()?;
        rest = &rest[end..];
        // The three components have to be separated by space, and a digit run
        // running straight into a letter is a word, not a colour.
        if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
            return None;
        }
    }
    Some(Swatch {
        rgb,
        name: rest.trim().to_owned(),
    })
}

// ---------------------------------------------------------------------------
// The library
// ---------------------------------------------------------------------------

/// Every palette the user has, one `.gpl` file each.
///
/// See the module docs for why it is a directory of the interchange format
/// rather than an index file.
#[derive(Clone, Debug, Default)]
pub struct PaletteLibrary {
    dir: PathBuf,
    /// Sorted by name — see [`Self::sort`].
    palettes: Vec<Palette>,
    /// Stems of `.gpl` files in the directory that are **not** in `palettes`:
    /// ones that would not parse, and ones past [`MAX_PALETTES`].
    ///
    /// Kept because a filename is an id here, and an id has to be free of every
    /// file in the directory rather than of every palette that happened to
    /// load. Without this, a file the library has just warned the user it could
    /// not read is a name [`Self::free_id`] hands straight out — and
    /// `write_atomically` renames over it. The artist is told their palette
    /// could not be read and then it is destroyed, silently, in the same
    /// session.
    occupied: Vec<String>,
    warnings: Vec<String>,
}

impl PaletteLibrary {
    /// Directory name under the platform's user-data directory, beside
    /// [`crate::preset::UserLibrary::DIR_NAME`].
    pub const DIR_NAME: &'static str = "palettes";

    /// `%APPDATA%\Umber\data\palettes` on Windows,
    /// `~/.local/share/umber/palettes` on Linux,
    /// `~/Library/Application Support/Umber/palettes` on macOS. `None` on a
    /// system with no home directory.
    pub fn default_dir() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "Umber")
            .map(|dirs| dirs.data_dir().join(Self::DIR_NAME))
    }

    pub fn load() -> Result<Self, PaletteError> {
        let dir = Self::default_dir().ok_or(PaletteError::NoDataDirectory)?;
        Ok(Self::load_from(dir))
    }

    /// Read every `.gpl` in a directory.
    ///
    /// Never fails. A missing directory is an empty library — the state every
    /// user starts in — and a file that will not parse is a warning rather than
    /// a refusal, for the reason `UserLibrary::load_tips` gives about an
    /// unreadable mask: one bad file must not put the whole collection out of
    /// reach.
    pub fn load_from(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let mut library = Self {
            dir,
            palettes: Vec::new(),
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
                    .is_some_and(|e| e.eq_ignore_ascii_case(GPL_EXTENSION))
            })
            .collect();
        // The directory's own order is whatever the filesystem felt like, and a
        // library that reads back differently on two machines is one whose
        // "first palette" is a different palette on each.
        paths.sort();
        let mut over = false;
        for path in paths {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if library.palettes.len() >= MAX_PALETTES {
                // Every remaining file still owns its name, so the loop carries
                // on collecting stems rather than breaking. One warning, not one
                // per file.
                if !over {
                    over = true;
                    library.warnings.push(format!(
                        "{} holds more than {MAX_PALETTES} palettes; the rest were not read",
                        library.dir.display()
                    ));
                }
                library.occupied.push(stem);
                continue;
            }
            match Self::read_file(&path) {
                Ok(palette) => library.palettes.push(palette),
                Err(e) => {
                    library.warnings.push(e.to_string());
                    library.occupied.push(stem);
                }
            }
        }
        library.sort();
        library
    }

    /// Read one file, taking its id from the filename.
    fn read_file(path: &Path) -> Result<Palette, PaletteError> {
        let text = fs::read_to_string(path).map_err(|source| PaletteError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let mut palette = read_gpl(&text, path)?.palette;
        palette.id = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(palette)
    }

    /// By name, case-folded, then by id.
    ///
    /// A list that reorders itself is a list whose rows move under the pointer,
    /// so this is not free — but the alternative is worse in both directions.
    /// Insertion order cannot survive a directory read, which has no order, and
    /// "newest last" would put a palette the artist has just made at the bottom
    /// of a list they then have to scroll. Sorting by the one thing the artist
    /// chose and can see means a rename moves a row, which is the one case
    /// where moving is what somebody just asked for.
    fn sort(&mut self) {
        self.palettes.sort_by(|a, b| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        });
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Anything that could not be read but did not stop the library loading.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn palettes(&self) -> &[Palette] {
        &self.palettes
    }

    pub fn get(&self, id: &str) -> Option<&Palette> {
        self.palettes.iter().find(|p| p.id == id)
    }

    pub fn is_empty(&self) -> bool {
        self.palettes.is_empty()
    }

    /// Add a palette, or replace the one that already has its id, and write
    /// that one file.
    ///
    /// Returns the id, which the caller needs when the palette was built by
    /// [`Palette::new`] and has none yet.
    /// Whether another palette will fit. The New control reads this and is
    /// *disabled* when it is false, rather than being live and refusing.
    pub fn has_room(&self) -> bool {
        self.palettes.len() < MAX_PALETTES
    }

    pub fn save(&mut self, mut palette: Palette) -> Result<String, PaletteError> {
        if palette.name.trim().is_empty() {
            palette.name = self.free_name(UNTITLED);
        }
        if palette.id.is_empty() {
            palette.id = self.free_id(&palette.name);
        }
        // Refused rather than written, and only for a palette that is *new*:
        // `load_from` stops reading at `MAX_PALETTES`, so one written past it
        // would be in the list this session and gone the next, with a warning
        // about a directory the artist did not know was full. Saving an edit to
        // a palette already here is always allowed — it writes no new file.
        let id = palette.id.clone();
        let known = self.palettes.iter().any(|p| p.id == id);
        if !known && !self.has_room() {
            return Err(PaletteError::Full { max: MAX_PALETTES });
        }
        self.write(&palette)?;
        match self.palettes.iter_mut().find(|p| p.id == id) {
            Some(existing) => *existing = palette,
            None => self.palettes.push(palette),
        }
        self.sort();
        Ok(id)
    }

    /// A new, empty palette with a name nothing else is called.
    ///
    /// Written out immediately, empty. A palette that existed only in memory
    /// until its first colour would be one the artist could name, see in the
    /// list, and lose by closing the window.
    pub fn create(&mut self, name: &str) -> Result<String, PaletteError> {
        let mut palette = Palette::new(self.free_name(name));
        palette.id = self.free_id(&palette.name);
        self.save(palette)
    }

    /// Take a palette out of the library and delete its file.
    ///
    /// `false` means there was nothing with that id, which is not an error — a
    /// double click on Delete should not raise a dialog.
    pub fn remove(&mut self, id: &str) -> Result<bool, PaletteError> {
        let Some(index) = self.palettes.iter().position(|p| p.id == id) else {
            return Ok(false);
        };
        let path = self.path_of(id);
        match fs::remove_file(&path) {
            Ok(()) => {}
            // Already gone is the outcome that was wanted.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(PaletteError::Io { path, source }),
        }
        self.palettes.remove(index);
        Ok(true)
    }

    /// Rename a palette. The id — and therefore the file — does not move.
    ///
    /// Deliberately: the id is what a selection holds, and renaming a file to
    /// match its title would orphan every reference to it in exchange for a
    /// tidier directory listing. It is the same rule [`crate::BrushPreset::id`]
    /// states, and the same reason.
    pub fn rename(&mut self, id: &str, name: &str) -> Result<(), PaletteError> {
        let taken: Vec<String> = self
            .palettes
            .iter()
            .filter(|p| p.id != id)
            .map(|p| p.name.clone())
            .collect();
        let name = unique_name(name, UNTITLED, taken.iter().map(String::as_str));
        let Some(palette) = self.palettes.iter().find(|p| p.id == id) else {
            return Err(PaletteError::Unknown(id.to_owned()));
        };
        let mut renamed = palette.clone();
        renamed.name = name;
        self.write(&renamed)?;
        if let Some(slot) = self.palettes.iter_mut().find(|p| p.id == id) {
            *slot = renamed;
        }
        self.sort();
        Ok(())
    }

    /// Read a `.gpl` from anywhere and put a copy in the library.
    ///
    /// Returns the new id and how many lines the file lost on the way in — see
    /// [`GplRead::skipped`].
    pub fn import(&mut self, path: &Path) -> Result<(String, usize), PaletteError> {
        let text = fs::read_to_string(path).map_err(|source| PaletteError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let read = read_gpl(&text, path)?;
        let mut palette = read.palette;
        // A name already in the library gets a number, rather than the import
        // quietly replacing a palette the artist built.
        palette.name = self.free_name(&palette.name);
        palette.id = String::new();
        let id = self.save(palette)?;
        Ok((id, read.skipped))
    }

    /// Write one palette out to a path of the caller's choosing.
    pub fn export(&self, id: &str, path: &Path) -> Result<(), PaletteError> {
        let palette = self
            .get(id)
            .ok_or_else(|| PaletteError::Unknown(id.to_owned()))?;
        write_atomically(path, &palette.to_gpl())
    }

    /// The file one id names.
    ///
    /// `slug` is applied here and not only where an id is *minted*, because
    /// [`Palette::id`] is a public field and this is the one place it becomes a
    /// path. "The callers only pass ids this module made" is not good enough —
    /// it is the standard [`crate::docformat`]'s reaper is held to, and the
    /// failure it prevents is a name with a separator in it writing outside the
    /// library. An id that is already a slug is unchanged by this, which is
    /// every id the library itself produced.
    fn path_of(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{}.{GPL_EXTENSION}", slug(id)))
    }

    fn write(&self, palette: &Palette) -> Result<(), PaletteError> {
        if let Err(source) = fs::create_dir_all(&self.dir) {
            return Err(PaletteError::Io {
                path: self.dir.clone(),
                source,
            });
        }
        write_atomically(&self.path_of(&palette.id), &palette.to_gpl())
    }

    /// A display name nothing else in the library is called.
    fn free_name(&self, desired: &str) -> String {
        unique_name(
            desired,
            UNTITLED,
            self.palettes.iter().map(|p| p.name.as_str()),
        )
    }

    /// A filename stem no file in the directory already occupies.
    ///
    /// Derived from the name so the directory is legible from a file manager,
    /// and reduced to what every filesystem accepts, so a palette called
    /// "Ochres / greys" cannot produce a path with a directory separator in it.
    ///
    /// Judged against **every `.gpl` that was seen**, not against the palettes
    /// that loaded — see [`Self::occupied`]. A file that would not parse still
    /// owns its name, and handing that name out means the next write renames
    /// over it: the artist is told their palette could not be read, and then it
    /// is destroyed.
    fn free_id(&self, name: &str) -> String {
        let base = slug(name);
        let taken: Vec<&str> = self
            .palettes
            .iter()
            .map(|p| p.id.as_str())
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
/// Deliberately narrow. The alternative is percent-encoding or trusting the
/// platform, and a palette whose file will not open on a machine the artist
/// copied it to is a worse outcome than one whose filename lost its accents —
/// the name the artist reads comes out of the file's `Name:` header, not out of
/// the path.
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
        "" => "palette".to_owned(),
        s => s.to_owned(),
    }
}

/// Write a file beside itself and rename over it.
///
/// The same trick `UserLibrary::write` uses, and for the same reason: a write
/// interrupted by a full disk or a pulled stick would otherwise leave a
/// truncated file where a palette used to be. Not `docformat::write_encoded`,
/// which is the *document's* atomic write — it reports a `SaveError`, and a
/// palette failing to save is not a document failing to save.
fn write_atomically(path: &Path, text: &str) -> Result<(), PaletteError> {
    let mut temporary = path.to_path_buf().into_os_string();
    temporary.push(".saving");
    let temporary = PathBuf::from(temporary);
    fs::write(&temporary, text).map_err(|source| PaletteError::Io {
        path: temporary.clone(),
        source,
    })?;
    if let Err(source) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(PaletteError::Io {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn here() -> PathBuf {
        PathBuf::from("test.gpl")
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("umber-palette-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    /// The one thing this module exists to guarantee: what the artist saved is
    /// byte for byte what comes back. It holds by construction because a swatch
    /// is stored in the form it is written in — which is the whole argument for
    /// eight-bit sRGB rather than a linear `Color`.
    #[test]
    fn a_palette_survives_being_written_and_read() {
        let mut palette = Palette::new("Ochres");
        palette.columns = 4;
        palette.swatches = vec![
            Swatch::new([0, 0, 0]),
            Swatch {
                rgb: [204, 119, 34],
                name: "Ochre".into(),
            },
            Swatch::new([255, 255, 255]),
        ];
        let read = read_gpl(&palette.to_gpl(), &here()).expect("its own output");
        assert_eq!(read.skipped, 0);
        assert_eq!(read.palette.name, "Ochres");
        assert_eq!(read.palette.columns, 4);
        assert_eq!(read.palette.swatches, palette.swatches);
    }

    /// Every byte a swatch can hold, through the colour the engine paints with
    /// and back. A palette that moved a level on the way to the canvas would be
    /// a picker that lies about what it is about to paint.
    #[test]
    fn a_swatch_and_the_engines_colour_are_exact_opposites() {
        for byte in 0..=255u8 {
            let swatch = Swatch::new([byte, 255 - byte, byte / 2]);
            assert_eq!(Swatch::of(swatch.colour()), swatch);
        }
        // And an engine colour that came from a swatch survives the trip out.
        let colour = Color::from_srgb_u8(17, 200, 90, 255);
        assert_eq!(Swatch::of(colour).colour(), colour);
    }

    /// Every other application writes `.gpl`, so Umber has to read what they
    /// write: a `Columns:` header, `#` comments, ragged spacing, names with
    /// spaces in them, and a trailing blank line.
    #[test]
    fn a_palette_another_application_wrote_reads() {
        let text = "GIMP Palette\nName: Someone's set\nColumns: 8\n#\n\
                    # made by hand\n  0   0   0\tBlack\n255 255 255 White smoke\n\
                    \t12\t34\t56\n\n";
        let read = read_gpl(text, &here()).expect("a plain gpl");
        assert_eq!(read.palette.name, "Someone's set");
        assert_eq!(read.palette.columns, 8);
        assert_eq!(read.skipped, 0);
        assert_eq!(read.palette.swatches.len(), 3);
        assert_eq!(read.palette.swatches[0].name, "Black");
        assert_eq!(read.palette.swatches[1].name, "White smoke");
        assert_eq!(read.palette.swatches[2].rgb, [12, 34, 56]);
        assert_eq!(read.palette.swatches[2].name, "");
    }

    /// A file that is not a palette is refused rather than read as an empty
    /// one — otherwise anything at all could be dragged into the library and
    /// would land there as a row with no colours in it.
    #[test]
    fn something_that_is_not_a_palette_is_refused() {
        assert!(matches!(
            read_gpl("<html>", &here()),
            Err(PaletteError::NotAPalette(_))
        ));
        assert!(matches!(
            read_gpl("", &here()),
            Err(PaletteError::NotAPalette(_))
        ));
    }

    /// A palette with a header and no colours is a palette, and refusing one
    /// would be the reader refusing its own output: `create` writes an empty
    /// palette so that naming one is enough to keep it, so a refusal here means
    /// every new palette vanishes on the next launch — with a dialog saying it
    /// had no colours in it.
    #[test]
    fn a_new_and_empty_palette_survives_being_closed_and_reopened() {
        let read = read_gpl("GIMP Palette\nName: Empty\n#\n", &here()).expect("still a palette");
        assert_eq!(read.palette.name, "Empty");
        assert!(read.palette.is_empty());

        let dir = temp_dir("empty");
        let mut library = PaletteLibrary::load_from(&dir);
        let id = library.create("Empty one").expect("made");
        let reopened = PaletteLibrary::load_from(&dir);
        assert!(reopened.warnings().is_empty(), "{:?}", reopened.warnings());
        let back = reopened.get(&id).expect("still there");
        assert_eq!(back.name, "Empty one");
        assert!(back.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    /// A line that is prose must not be read as a colour, and a component out
    /// of range must not be clamped into one — both would put a colour nobody
    /// chose into the artist's palette. They are *counted*, because an import
    /// that loses something has to say so.
    #[test]
    fn lines_that_are_not_colours_are_skipped_and_counted() {
        let text = "GIMP Palette\nName: Mixed\n#\n1999 Vintage Reds\n\
                    300 0 0\n10 20\n  1   2   3\n";
        let read = read_gpl(text, &here()).expect("one real colour");
        assert_eq!(read.palette.swatches, vec![Swatch::new([1, 2, 3])]);
        assert_eq!(read.skipped, 3, "three lines were not colours");
    }

    /// A name with a newline in it would write a file whose next line is read
    /// as a colour — or silently end the palette there. `.gpl` has no escaping,
    /// so the only answer is not to write one.
    #[test]
    fn a_name_cannot_break_the_file_format() {
        let mut palette = Palette::new("Two\nlines");
        palette.swatches.push(Swatch {
            rgb: [1, 2, 3],
            name: "a\tname\nwith\rbreaks".into(),
        });
        let text = palette.to_gpl();
        let read = read_gpl(&text, &here()).expect("still a palette");
        assert_eq!(read.palette.name, "Two lines");
        assert_eq!(read.palette.swatches.len(), 1);
        assert_eq!(read.palette.swatches[0].name, "a name with breaks");
        assert_eq!(read.skipped, 0);
    }

    /// A palette read out of a file with no `Name:` takes the filename, which
    /// is what every other reader shows for one.
    #[test]
    fn a_nameless_palette_is_called_after_its_file() {
        let read = read_gpl("GIMP Palette\n#\n1 2 3\n", Path::new("/tmp/Warm greys.gpl"))
            .expect("a palette");
        assert_eq!(read.palette.name, "Warm greys");
    }

    #[test]
    fn a_palette_is_bounded() {
        let mut text = String::from("GIMP Palette\nName: Huge\n#\n");
        for _ in 0..MAX_SWATCHES + 1 {
            text.push_str("1 2 3\n");
        }
        assert!(matches!(
            read_gpl(&text, &here()),
            Err(PaletteError::TooManySwatches { .. })
        ));

        let mut palette = Palette::new("Full");
        for _ in 0..MAX_SWATCHES {
            assert!(palette.add(Swatch::new([0, 0, 0])));
        }
        assert!(!palette.has_room());
        assert!(!palette.add(Swatch::new([1, 1, 1])), "refused, not silent");
        assert_eq!(palette.len(), MAX_SWATCHES);
    }

    /// A file stem has to be something every filesystem accepts, whatever the
    /// artist typed — a name with a separator in it would otherwise write the
    /// palette into a directory that is not the library.
    #[test]
    fn an_id_is_a_filename_and_nothing_more() {
        for (name, want) in [
            ("Ochres", "ochres"),
            ("Warm  greys", "warm-greys"),
            ("Ochres / greys", "ochres-greys"),
            ("../../etc", "etc"),
            ("   ", "palette"),
            ("日本", "palette"),
            ("C:\\x", "c-x"),
        ] {
            assert_eq!(slug(name), want, "{name}");
        }
        assert!(slug(&"a".repeat(200)).len() <= 48);
    }

    /// Two palettes may not share a file, whatever they are called — and the
    /// tie is broken here rather than by whichever platform the library is
    /// opened on, because two ids differing only in case are two files on Linux
    /// and one on Windows.
    #[test]
    fn two_palettes_never_share_a_file() {
        let dir = temp_dir("ids");
        let mut library = PaletteLibrary::load_from(&dir);
        let a = library.create("Ochres").expect("first");
        let b = library.create("Ochres").expect("second");
        let c = library.create("OCHRES").expect("third");
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
        // And the names are told apart too, so the list is readable.
        let names: Vec<&str> = library.palettes().iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"Ochres"));
        assert!(names.contains(&"Ochres 2"));
        let _ = fs::remove_dir_all(&dir);
    }

    /// The whole library, round-tripped through a directory: what was saved is
    /// what loads, and a delete takes the file with it.
    #[test]
    fn a_library_is_the_directory_it_reads_back() {
        let dir = temp_dir("roundtrip");
        let mut library = PaletteLibrary::load_from(&dir);
        assert!(
            library.is_empty(),
            "a missing directory is an empty library"
        );

        let id = library.create("Ochres").expect("made");
        let mut palette = library.get(&id).expect("in the library").clone();
        palette.add(Swatch::of(Color::from_srgb_u8(204, 119, 34, 255)));
        library.save(palette).expect("written");

        let reopened = PaletteLibrary::load_from(&dir);
        assert!(reopened.warnings().is_empty(), "{:?}", reopened.warnings());
        let back = reopened.get(&id).expect("still there");
        assert_eq!(back.name, "Ochres");
        assert_eq!(back.swatches, vec![Swatch::new([204, 119, 34])]);

        // The id is what a selection holds, so a rename must not move it.
        let mut library = reopened;
        library.rename(&id, "Earths").expect("renamed");
        assert_eq!(library.get(&id).expect("same id").name, "Earths");
        assert_eq!(
            PaletteLibrary::load_from(&dir).get(&id).unwrap().name,
            "Earths"
        );

        assert!(library.remove(&id).expect("deleted"));
        assert!(!library.remove(&id).expect("already gone is not an error"));
        assert!(PaletteLibrary::load_from(&dir).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    /// A file the library could not read still owns its name. Handing that name
    /// out means the next write renames over it — so the artist is told their
    /// palette could not be read, and then it is destroyed, in the same session
    /// and with no second warning.
    #[test]
    fn a_file_that_would_not_read_is_never_written_over() {
        let dir = temp_dir("occupied");
        fs::create_dir_all(&dir).expect("a directory");
        // A file whose stem is exactly what `create("Bad")` would mint.
        fs::write(dir.join("bad.gpl"), "this is not a palette").unwrap();
        let before = fs::read_to_string(dir.join("bad.gpl")).unwrap();

        let mut library = PaletteLibrary::load_from(&dir);
        assert_eq!(library.warnings().len(), 1, "it said it could not read it");
        let id = library.create("Bad").expect("made");
        assert_ne!(id, "bad", "the stem was already taken");
        assert_eq!(
            fs::read_to_string(dir.join("bad.gpl")).unwrap(),
            before,
            "the file the library warned about was overwritten"
        );
        assert!(dir.join(format!("{id}.gpl")).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// A palette written past what `load_from` will read back would be in the
    /// list this session and gone the next, with a warning about a directory
    /// nobody was told was full. It is refused instead, and the control that
    /// offers it reads `has_room`.
    #[test]
    fn the_library_refuses_a_palette_it_could_not_read_back() {
        let dir = temp_dir("full");
        let mut library = PaletteLibrary::load_from(&dir);
        for n in 0..MAX_PALETTES {
            library.create(&format!("P{n}")).expect("room");
        }
        assert!(!library.has_room());
        assert!(matches!(
            library.create("One too many"),
            Err(PaletteError::Full { .. })
        ));
        // An edit to a palette already here is always allowed: it writes no new
        // file, so nothing is lost on the next launch.
        let id = library.palettes()[0].id.clone();
        let mut palette = library.get(&id).expect("there").clone();
        palette.add(Swatch::new([1, 2, 3]));
        library.save(palette).expect("an edit still saves");
        assert_eq!(library.get(&id).expect("there").len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    /// One unreadable file must not put the whole collection out of reach, and
    /// the artist has to be told which one it was.
    #[test]
    fn a_file_that_will_not_parse_is_a_warning_rather_than_a_refusal() {
        let dir = temp_dir("broken");
        fs::create_dir_all(&dir).expect("a directory");
        fs::write(dir.join("good.gpl"), "GIMP Palette\nName: Good\n#\n1 2 3\n").unwrap();
        fs::write(dir.join("bad.gpl"), "not a palette at all").unwrap();
        // Something that is not a palette file at all is not even looked at.
        fs::write(dir.join("notes.txt"), "hello").unwrap();

        let library = PaletteLibrary::load_from(&dir);
        assert_eq!(library.palettes().len(), 1);
        assert_eq!(library.palettes()[0].name, "Good");
        assert_eq!(library.warnings().len(), 1);
        assert!(
            library.warnings()[0].contains("bad.gpl"),
            "{:?}",
            library.warnings()
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// An import lands beside what is already there rather than replacing a
    /// palette that happens to share its name.
    #[test]
    fn an_import_never_overwrites_a_palette_that_is_already_there() {
        let dir = temp_dir("import");
        let mut library = PaletteLibrary::load_from(&dir);
        let mine = library.create("Ochres").expect("made");

        let incoming = dir.join("incoming.gpl");
        fs::write(
            &incoming,
            "GIMP Palette\nName: Ochres\n#\n9 9 9\nnonsense\n",
        )
        .unwrap();
        let (imported, skipped) = library.import(&incoming).expect("read");
        assert_ne!(imported, mine);
        assert_eq!(skipped, 1, "the import says what it dropped");
        assert_eq!(library.palettes().len(), 2);
        assert_eq!(library.get(&mine).expect("untouched").swatches, vec![]);
        assert_eq!(
            library.get(&imported).expect("the new one").swatches,
            vec![Swatch::new([9, 9, 9])]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// An export is a `.gpl` anything else can open, and reading it back gives
    /// the same colours.
    #[test]
    fn an_export_is_a_file_every_other_application_reads() {
        let dir = temp_dir("export");
        let mut library = PaletteLibrary::load_from(&dir);
        let id = library.create("Ochres").expect("made");
        let mut palette = library.get(&id).unwrap().clone();
        palette.add(Swatch::new([204, 119, 34]));
        library.save(palette).expect("saved");

        let out = dir.join("elsewhere.gpl");
        library.export(&id, &out).expect("exported");
        let text = fs::read_to_string(&out).unwrap();
        assert!(text.starts_with(GPL_HEADER), "{text}");
        let read = read_gpl(&text, &out).expect("a palette");
        assert_eq!(read.palette.swatches, vec![Swatch::new([204, 119, 34])]);
        assert!(matches!(
            library.export("nothing", &out),
            Err(PaletteError::Unknown(_))
        ));
        let _ = fs::remove_dir_all(&dir);
    }
}
