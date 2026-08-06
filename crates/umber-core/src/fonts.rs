//! Which faces this machine can set text in.
//!
//! # Why the machine's own fonts are the feature
//!
//! The request this answers was "every open source or licence-allowing font we
//! can find". Taken literally that is the Google Fonts catalogue, which is
//! **about 1.1 GB** against an `assets/` tree of 59 MB — and Umber updates
//! itself in place by fetching a whole release archive, so anything bundled is
//! paid for by every user on every platform on every update. `docs/text-tool.md`
//! §2 has the arithmetic.
//!
//! The other half of the problem is that "licence-allowing" does an enormous
//! amount of work in that sentence. Most of what an aggregator calls free is
//! free *for personal use* and carries no redistribution right at all, and a
//! system font — Segoe UI, Calibri, SF Pro — is licensed to the machine rather
//! than to a redistributor. **Every font already installed is already licensed
//! to the person using it**, including the ones they bought and the ones the
//! operating system came with, which no bundle could legally contain.
//! Enumerating them ships nothing, redistributes nothing, and is the only route
//! to the faces somebody actually owns.
//!
//! So there are three sources and they are deliberately in this order:
//!
//! 1. **Every font installed on the machine** — [`search_roots`].
//! 2. **A folder the user points Umber at**, for a foundry licence or a work
//!    library. Umber reads it and **copies nothing out of it**: the moment it
//!    did, it would be redistributing, inside somebody's own documents folder.
//! 3. **What is compiled in**, which is Archivo alone — the typeface the
//!    interface is drawn in. It is registered by the caller through
//!    [`FontLibrary::add_builtin`] rather than included here a third time; the
//!    bytes are already in the binary twice and a third copy buys nothing.
//!
//! A curated OFL/Apache bundle beside Archivo is **designed and not built**.
//! `docs/text-tool.md` §2 has it: ten to twenty families at the measured half a
//! megabyte each, fetched by a `tools/fetch-fonts` pair on `fetch-brushes`'
//! pattern, refusing anything whose licence cannot be verified *inside* the
//! download. That argument is worth keeping, because it is the shape the thing
//! would have to take. What is on disk today is `assets/fonts/` holding one
//! typeface: there is no fetch script and no `bundled/` directory, and this
//! comment claimed both until somebody looked. Wiring a bundle into the
//! *binary* would be a further decision again, and is not made here.
//!
//! # Why this is not `fontdb`
//!
//! `fontdb` is the obvious crate and would do the walk. What it costs is six or
//! seven new dependencies for a directory listing and a `name` table read, on a
//! project that argues about every crate in the manifest — and it would not make
//! the *rule* testable, which is the part that actually goes wrong. [`Probe`] is
//! the same shape `install::detect` keeps: the platform, the home directory and
//! the two Windows environment variables are **injected**, so the macOS and
//! Linux answers are checked on a Windows machine, which is the only way they
//! are checked at all. Reading the faces out of the files is `skrifa`'s, and
//! `skrifa` was already a direct dependency.
//!
//! # What a scan costs, and when it may happen
//!
//! Every file is opened, parsed far enough to read its family and style, and
//! **closed again** — a `FontLibrary` holds paths, not bytes, and
//! [`Face::load`] reads the one file that is about to be drawn with. A typical
//! Windows installation is several hundred files and a few hundred megabytes of
//! I/O, which is fine once on a worker thread and is exactly the sort of thing
//! that must never happen on the drawing path. Nothing here spawns that thread;
//! the caller does, because a thread is the platform's and this crate is not.

use skrifa::attribute::Style;
use skrifa::raw::FileRef;
use skrifa::string::StringId;
use skrifa::{FontRef, MetadataProvider};
use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// How deep into a font directory a scan will walk.
///
/// Bounded rather than unbounded because these are directories nobody here
/// controls: `/usr/share/fonts` is two or three levels on every distribution
/// that has ever shipped, and a user's own folder pointed at their home
/// directory by accident must not walk the whole disk. Four is comfortably past
/// what any real layout uses.
const MAX_DEPTH: usize = 4;

/// The most files one scan will look at.
///
/// The same reasoning, from the other end: the depth limit does not bound a
/// single directory with fifty thousand files in it. A limit reached is logged
/// by the caller and leaves a usable library rather than a hung application.
const MAX_FILES: usize = 20_000;

/// Which platform's font directories to name.
///
/// Injected rather than read from `cfg!`, so all three answers are testable
/// wherever the tests happen to run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Windows,
    MacOs,
    /// Linux, the BSDs, and anything else with an X or Wayland session.
    Unix,
}

/// Everything [`search_roots`] is allowed to know about the machine.
#[derive(Clone, Debug, Default)]
pub struct Probe {
    pub os: Option<Os>,
    /// The user's home directory.
    pub home: Option<PathBuf>,
    /// Windows' `%SystemRoot%`. Not assumed to be `C:\Windows`: it is not, on
    /// machines that were installed onto another volume.
    pub windir: Option<PathBuf>,
    /// Windows' `%LOCALAPPDATA%`, which is where a font installed for one user
    /// rather than for the machine goes.
    pub local_app_data: Option<PathBuf>,
    /// `$XDG_DATA_HOME`, which overrides `~/.local/share` when it is set.
    pub xdg_data_home: Option<PathBuf>,
}

impl Probe {
    /// What this machine actually answers.
    ///
    /// The one function here that reads the environment, so everything else
    /// stays a pure function of a reading — see the module docs.
    pub fn here() -> Self {
        let os = if cfg!(target_os = "windows") {
            Some(Os::Windows)
        } else if cfg!(target_os = "macos") {
            Some(Os::MacOs)
        } else if cfg!(any(target_os = "android", target_os = "ios")) {
            // Neither has ever been built or run, and neither keeps its fonts
            // where a desktop does. Answering `Unix` would send a scan walking
            // directories that are not there; answering nothing leaves the
            // built-in face, which is honest.
            None
        } else {
            Some(Os::Unix)
        };
        Self {
            os,
            home: std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(PathBuf::from),
            windir: std::env::var_os("SystemRoot")
                .or_else(|| std::env::var_os("windir"))
                .map(PathBuf::from),
            local_app_data: std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
            xdg_data_home: std::env::var_os("XDG_DATA_HOME").map(PathBuf::from),
        }
    }
}

/// Every directory this machine keeps fonts in, most general first.
///
/// A root that does not exist is still returned: whether it is there is the
/// scanner's question, and keeping the two apart is what lets this be a pure
/// function.
pub fn search_roots(probe: &Probe) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut push = |p: PathBuf| {
        if !roots.contains(&p) {
            roots.push(p);
        }
    };
    match probe.os {
        None => {}
        Some(Os::Windows) => {
            if let Some(windir) = &probe.windir {
                push(windir.join("Fonts"));
            }
            // Per-user installs, which is where anything installed without
            // administrator rights since Windows 10 1803 goes.
            if let Some(local) = &probe.local_app_data {
                push(local.join("Microsoft").join("Windows").join("Fonts"));
            }
        }
        Some(Os::MacOs) => {
            push(PathBuf::from("/System/Library/Fonts"));
            // Where every face that is not in the boot minimum has lived since
            // Catalina. Missing it means missing most of the system's fonts.
            push(PathBuf::from("/System/Library/Fonts/Supplemental"));
            push(PathBuf::from("/Library/Fonts"));
            if let Some(home) = &probe.home {
                push(home.join("Library").join("Fonts"));
            }
        }
        Some(Os::Unix) => {
            push(PathBuf::from("/usr/share/fonts"));
            push(PathBuf::from("/usr/local/share/fonts"));
            // `$XDG_DATA_HOME/fonts` where it is set, and `~/.local/share/fonts`
            // where it is not — the specification's own fallback, so a session
            // that sets the variable is not scanned twice.
            match &probe.xdg_data_home {
                Some(xdg) => push(xdg.join("fonts")),
                None => {
                    if let Some(home) = &probe.home {
                        push(home.join(".local").join("share").join("fonts"));
                    }
                }
            }
            if let Some(home) = &probe.home {
                // The pre-XDG location, still populated on plenty of machines.
                push(home.join(".fonts"));
            }
        }
    }
    roots
}

/// Whether a path names a font file this can read.
///
/// Extension only, and deliberately: the alternative is opening every file in
/// `/usr/share/fonts`, which on a machine with a large bitmap-font collection is
/// tens of thousands of pointless reads. `.pfb`, `.woff` and `.woff2` are left
/// out because nothing here can parse them — naming a face Umber cannot then
/// draw with is the control that lies.
fn is_font_file(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "ttf" | "otf" | "ttc" | "otc"
    )
}

/// Where a face's bytes come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// Compiled into the binary. The `&'static str` is the name it was
    /// registered under, which is what a preference records.
    Builtin(&'static str),
    /// A file on this machine, and which face of it — a `.ttc` holds several.
    File { path: PathBuf, index: u32 },
}

/// One face: a family, a style within it, and where to read it from.
#[derive(Clone, Debug, PartialEq)]
pub struct Face {
    pub family: String,
    /// "Regular", "Bold Italic", "Condensed Light" — the font's own subfamily
    /// name, not one derived from the weight.
    pub style: String,
    /// OS/2 `usWeightClass`, 1..=1000. 400 is regular and 700 is bold.
    pub weight: u16,
    pub italic: bool,
    pub source: Source,
    /// Where on a variable font's axes this style sits — `[("wght", 700.0)]`
    /// and so on — empty for a static face and for a variable font's own
    /// default instance.
    ///
    /// This is why a *style* is not just a name. A variable font is one file
    /// carrying a continuum, and the styles somebody expects to pick from are
    /// its **named instances**; without them Archivo offers "Regular" alone
    /// while the file it comes out of holds nine weights and every width. It is
    /// also the reason `cputext.rs` exists at all — `ab_glyph` ignores
    /// variation axes and renders the default master whatever weight is asked
    /// for, which is why the interface has no bold.
    pub variations: Vec<(String, f32)>,
}

impl Face {
    /// Read the bytes this face is made of.
    ///
    /// Blocking, and the whole reason a [`FontLibrary`] holds paths rather than
    /// data: a scan of several hundred faces that kept every one of them open
    /// would be hundreds of megabytes resident to draw one caption.
    pub fn load(&self) -> Option<FontData> {
        match &self.source {
            Source::Builtin(name) => builtin_bytes(name).map(|bytes| FontData {
                bytes: Cow::Borrowed(bytes),
                index: 0,
            }),
            Source::File { path, index } => std::fs::read(path).ok().map(|bytes| FontData {
                bytes: Cow::Owned(bytes),
                index: *index,
            }),
        }
    }

    /// How a face is named in a list: the style alone, because the family is
    /// the row above it.
    pub fn label(&self) -> &str {
        &self.style
    }
}

/// The bytes of one face, held long enough to draw with.
#[derive(Clone, Debug)]
pub struct FontData {
    bytes: Cow<'static, [u8]>,
    index: u32,
}

impl FontData {
    /// Parse. `None` for a file that is not a font after all — which happens,
    /// because the scan reads extensions rather than magic numbers.
    pub fn font(&self) -> Option<FontRef<'_>> {
        FontRef::from_index(&self.bytes, self.index).ok()
    }

    /// The bytes, for a caller that wants to parse them itself.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Faces compiled into the binary, registered by the caller.
///
/// A `Mutex` rather than a `OnceLock` of a table because the caller registers
/// at start-up and the table is read from wherever a face is loaded; there is
/// exactly one writer and it runs before any reader, so this is never
/// contended. It is the same shape `history::set_default_budget` uses, and for
/// the same reason: threading the bytes through every constructor that opens a
/// picture would put a font into the signature of everything.
static BUILTIN: std::sync::Mutex<Vec<(&'static str, &'static [u8])>> =
    std::sync::Mutex::new(Vec::new());

fn builtin_bytes(name: &str) -> Option<&'static [u8]> {
    let table = BUILTIN.lock().ok()?;
    table.iter().find(|(n, _)| *n == name).map(|(_, b)| *b)
}

/// Every face Umber can set text in, grouped by family.
#[derive(Clone, Debug, Default)]
pub struct FontLibrary {
    /// Sorted by family (case-insensitively), then by weight, then upright
    /// before italic — so a family's styles read in the order somebody expects
    /// to find them in.
    faces: Vec<Face>,
    /// How many files the scan looked at, and how many it could not read. Shown
    /// in the interface rather than logged alone: a scan that found forty faces
    /// where the machine has four hundred is a thing somebody has to be able to
    /// see.
    pub scanned: usize,
    pub unreadable: usize,
}

impl FontLibrary {
    /// Register a face that is compiled into the binary and put it in the
    /// library.
    ///
    /// The caller owns the bytes because they are already in the binary — see
    /// the module docs. Registering the same name twice keeps the first.
    pub fn add_builtin(&mut self, name: &'static str, bytes: &'static [u8]) {
        {
            let Ok(mut table) = BUILTIN.lock() else {
                return;
            };
            if !table.iter().any(|(n, _)| *n == name) {
                table.push((name, bytes));
            }
        }
        let data = FontData {
            bytes: Cow::Borrowed(bytes),
            index: 0,
        };
        if let Some(font) = data.font() {
            for face in read_faces(&font, &Source::Builtin(name)) {
                self.insert(face);
            }
        }
    }

    /// Walk `roots`, adding every face found.
    ///
    /// Blocking and slow — see the module docs. Roots that do not exist are
    /// skipped in silence, because [`search_roots`] deliberately names every
    /// directory a platform *might* keep fonts in.
    pub fn scan(&mut self, roots: &[PathBuf]) {
        let mut files = Vec::new();
        for root in roots {
            collect(root, 0, &mut files);
            if files.len() >= MAX_FILES {
                break;
            }
        }
        files.sort();
        files.dedup();
        self.scanned += files.len();
        for path in files {
            if !self.add_file(&path) {
                self.unreadable += 1;
            }
        }
    }

    /// Add every face in one file. False when nothing could be read out of it.
    pub fn add_file(&mut self, path: &Path) -> bool {
        let Ok(bytes) = std::fs::read(path) else {
            return false;
        };
        let Ok(file) = FileRef::new(&bytes) else {
            return false;
        };
        let mut found = 0;
        for (index, font) in file.fonts().enumerate() {
            let Ok(font) = font else { continue };
            let source = Source::File {
                path: path.to_path_buf(),
                index: index as u32,
            };
            for face in read_faces(&font, &source) {
                self.insert(face);
                found += 1;
            }
        }
        found > 0
    }

    /// Insert keeping the sort order, and refuse a duplicate.
    ///
    /// Duplicates are the ordinary case rather than an oddity: a Linux machine
    /// routinely has the same family in `/usr/share/fonts` and in the user's own
    /// directory, and a list that offered "DejaVu Sans — Book" twice would be a
    /// list nobody trusts. The **first** wins, which with [`search_roots`]'s
    /// order means the system copy rather than a stray one.
    fn insert(&mut self, face: Face) {
        let key = |f: &Face| {
            (
                f.family.to_lowercase(),
                f.weight,
                f.italic,
                f.style.to_lowercase(),
            )
        };
        let k = key(&face);
        match self.faces.binary_search_by(|f| key(f).cmp(&k)) {
            Ok(_) => {}
            Err(at) => self.faces.insert(at, face),
        }
    }

    pub fn faces(&self) -> &[Face] {
        &self.faces
    }

    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }

    /// Every family name, in the order the list draws them.
    ///
    /// **Grouped case-insensitively, because that is how the list is sorted.**
    /// Two files naming one typeface with different capitals is an ordinary
    /// thing to find on a real disk — foundries are inconsistent about it
    /// between weight files, and so are the people who rename them — and those
    /// faces sort into one contiguous run under [`Self::insert`]'s lowercased
    /// key. Comparing the raw names here would then start a *second* row part
    /// way through that run, and [`Self::family`] would hand each row only its
    /// own spelling: half a typeface's weights hidden behind a picker that
    /// looked complete. The first spelling met is the one shown.
    pub fn families(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for face in &self.faces {
            if out
                .last()
                .is_none_or(|last| !last.eq_ignore_ascii_case(&face.family))
            {
                out.push(&face.family);
            }
        }
        out
    }

    /// The faces of one family, in style order.
    ///
    /// Case-insensitive, for the reason [`Self::families`] gives: the name this
    /// is called with came out of that list, and the run it names may hold more
    /// than one spelling.
    pub fn family(&self, name: &str) -> Vec<&Face> {
        self.faces
            .iter()
            .filter(|f| f.family.eq_ignore_ascii_case(name))
            .collect()
    }

    /// The face a `(family, style)` pair names, or the nearest thing to it.
    ///
    /// Total by construction, because everything downstream of it is: a
    /// preference records a family and a style by *name*, and the machine it is
    /// read back on may not have either. So an exact match first, then anything
    /// in that family — nearest weight to regular, upright before italic —
    /// then the first face in the library, which is the built-in one. The
    /// caller is what says a substitution happened; this only refuses when the
    /// library is empty.
    pub fn resolve(&self, family: &str, style: &str) -> Option<&Face> {
        // Case-insensitively on both halves, for the reason [`Self::families`]
        // gives about the family and one more about the style: the style is
        // stored in a preferences file and typed back by whoever edits it.
        if let Some(exact) = self
            .faces
            .iter()
            .find(|f| f.family.eq_ignore_ascii_case(family) && f.style.eq_ignore_ascii_case(style))
        {
            return Some(exact);
        }
        let nearest = self
            .faces
            .iter()
            .filter(|f| f.family.eq_ignore_ascii_case(family))
            .min_by_key(|f| (f.italic, f.weight.abs_diff(400)));
        nearest.or_else(|| self.faces.first())
    }
}

/// Walk one directory, pushing every font file into `out`.
///
/// Recursive, and [`MAX_DEPTH`] is what makes that safe rather than a hope: a
/// symbolic link pointing at its own parent is a real thing to find in a font
/// directory, and without the bound it is an unbounded recursion that ends in a
/// stack overflow rather than in a shorter list. **Anyone raising `MAX_DEPTH`
/// is raising a stack depth**, which is the sentence this comment exists for.
fn collect(root: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    // `>=`, so `MAX_DEPTH` is the number of levels walked rather than one less
    // than it: the root is depth 0, and the deepest directory opened is
    // `MAX_DEPTH - 1`.
    if depth >= MAX_DEPTH || out.len() >= MAX_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= MAX_FILES {
            return;
        }
        let path = entry.path();
        // `file_type` rather than `metadata`, so a link is seen as a link
        // rather than as whatever it points at. Following one is what makes the
        // loop above possible at all.
        match entry.file_type() {
            Ok(t) if t.is_dir() => collect(&path, depth + 1, out),
            Ok(t) if t.is_file() && is_font_file(&path) => out.push(path),
            _ => {}
        }
    }
}

/// Every style one font offers: its default instance, and one per **named
/// instance** where it is a variable font.
///
/// Empty where there is no usable family name, which is the one thing a face
/// cannot be listed without.
///
/// The named instances are not a refinement. A modern system font is one file
/// carrying a whole family — Archivo is nine weights and every width in
/// 643 KB — so reading the default instance alone offers "Regular" and hides
/// the rest of the typeface behind a picker that looks like it is working.
fn read_faces(font: &FontRef, source: &Source) -> Vec<Face> {
    let Some(base) = read_face(font, source) else {
        return Vec::new();
    };
    let axes: Vec<(String, f32)> = font
        .axes()
        .iter()
        .map(|a| (a.tag().to_string(), a.default_value()))
        .collect();
    let mut out = vec![base.clone()];
    for instance in font.named_instances().iter() {
        let Some(style) = string(font, instance.subfamily_name_id()) else {
            continue;
        };
        let variations: Vec<(String, f32)> = axes
            .iter()
            .map(|(tag, _)| tag.clone())
            .zip(instance.user_coords())
            .collect();
        // The instance that *is* the default is the base face already listed,
        // and it must be recorded with no variations: `Face::variations` being
        // empty is what tells `text::set` there is nothing to instance, which
        // is the fast path and the exact identity.
        let is_default = variations
            .iter()
            .zip(&axes)
            .all(|((_, v), (_, d))| (v - d).abs() < f32::EPSILON);
        // A weight axis names the weight far better than OS/2 does for an
        // instance, because OS/2 describes the default and never moves.
        let weight = variations
            .iter()
            .find(|(tag, _)| tag == "wght")
            .map(|(_, v)| v.round().clamp(1.0, 1000.0) as u16)
            .unwrap_or(base.weight);
        let italic = variations
            .iter()
            .find(|(tag, _)| tag == "ital")
            .map(|(_, v)| *v >= 0.5)
            .unwrap_or_else(|| {
                variations
                    .iter()
                    .find(|(tag, _)| tag == "slnt")
                    .map(|(_, v)| v.abs() > 0.5)
                    .unwrap_or(base.italic)
            });
        out.push(Face {
            family: base.family.clone(),
            style,
            weight,
            italic,
            source: source.clone(),
            variations: if is_default { Vec::new() } else { variations },
        });
    }
    out
}

/// Read one font's family and style out of its `name` table.
///
/// `None` where there is no usable family name, which is the one thing a face
/// cannot be listed without.
fn read_face(font: &FontRef, source: &Source) -> Option<Face> {
    // The **typographic** family (name ID 16) where the font states one, and the
    // legacy family (ID 1) otherwise. The two differ exactly where it matters:
    // a large family is split across several ID-1 families of at most four
    // styles each — "Archivo", "Archivo Light", "Archivo SemiBold" — because
    // that is all the Windows GDI menu could carry, and listing those as
    // separate families is how a font picker ends up with nine entries for one
    // typeface.
    let family = string(font, StringId::TYPOGRAPHIC_FAMILY_NAME)
        .or_else(|| string(font, StringId::FAMILY_NAME))?;
    let attrs = font.attributes();
    let italic = !matches!(attrs.style, Style::Normal);
    let style = string(font, StringId::TYPOGRAPHIC_SUBFAMILY_NAME)
        .or_else(|| string(font, StringId::SUBFAMILY_NAME))
        .unwrap_or_else(|| "Regular".to_string());
    Some(Face {
        family,
        style,
        weight: attrs.weight.value().round().clamp(1.0, 1000.0) as u16,
        italic,
        source: source.clone(),
        variations: Vec::new(),
    })
}

/// One name-table string, trimmed, or `None` when it is absent or empty.
///
/// The first record wins. Picking the English one instead is the obvious
/// refinement and is not free — the `name` table's language is a platform
/// specific id, so it means a table of Macintosh and Windows language codes —
/// and the first record is the English one in every font this has been run
/// against. What it must not do is *fail*: a face named only in Japanese is a
/// face somebody wants to use.
fn string(font: &FontRef, id: StringId) -> Option<String> {
    let s = font.localized_strings(id).next()?.to_string();
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// The one face that is certainly there, for the tests in this crate.
///
/// The same file the interface is drawn in, and the same one `umber-app`
/// registers through [`FontLibrary::add_builtin`] at start-up. Behind
/// `cfg(test)` because `umber-core` does **not** compile a font in — the bytes
/// are already in the binary twice and a third copy for a library that is
/// handed them anyway would be a quarter of a megabyte for nothing.
#[cfg(test)]
pub(crate) const TEST_FONT: &[u8] = include_bytes!("../../../assets/fonts/Archivo[wdth,wght].ttf");

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(os: Os) -> Probe {
        Probe {
            os: Some(os),
            home: Some(PathBuf::from("/home/painter")),
            windir: Some(PathBuf::from("D:\\Windows")),
            local_app_data: Some(PathBuf::from("D:\\Users\\painter\\AppData\\Local")),
            xdg_data_home: None,
        }
    }

    fn names(roots: &[PathBuf]) -> Vec<String> {
        roots
            .iter()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .collect()
    }

    /// The whole point of injecting the reading: all three platforms' answers
    /// are checked wherever the tests happen to run, which is the only way the
    /// two nobody here uses are checked at all.
    #[test]
    fn every_platforms_font_directories_are_named() {
        let win = names(&search_roots(&probe(Os::Windows)));
        assert!(win.contains(&"D:/Windows/Fonts".to_string()), "{win:?}");
        assert!(
            win.iter()
                .any(|p| p.ends_with("AppData/Local/Microsoft/Windows/Fonts")),
            "per-user installs are missed: {win:?}"
        );

        let mac = names(&search_roots(&probe(Os::MacOs)));
        assert!(
            mac.contains(&"/System/Library/Fonts".to_string()),
            "{mac:?}"
        );
        // Everything past the boot minimum has lived here since Catalina;
        // missing it misses most of the system's faces.
        assert!(
            mac.contains(&"/System/Library/Fonts/Supplemental".to_string()),
            "{mac:?}"
        );
        assert!(mac.contains(&"/home/painter/Library/Fonts".to_string()));

        let unix = names(&search_roots(&probe(Os::Unix)));
        assert!(unix.contains(&"/usr/share/fonts".to_string()), "{unix:?}");
        assert!(unix.contains(&"/home/painter/.local/share/fonts".to_string()));
        assert!(unix.contains(&"/home/painter/.fonts".to_string()));
    }

    /// `$XDG_DATA_HOME` replaces the default rather than being scanned beside
    /// it — the specification's own rule, and it is also what stops a session
    /// that sets the variable to its default value scanning that tree twice.
    #[test]
    fn xdg_data_home_replaces_the_default_it_stands_for() {
        let mut p = probe(Os::Unix);
        p.xdg_data_home = Some(PathBuf::from("/home/painter/data"));
        let roots = names(&search_roots(&p));
        assert!(
            roots.contains(&"/home/painter/data/fonts".to_string()),
            "{roots:?}"
        );
        assert!(
            !roots.contains(&"/home/painter/.local/share/fonts".to_string()),
            "scanned twice: {roots:?}"
        );
    }

    /// A platform with no answer gets no roots at all, rather than being walked
    /// as though it were a desktop. Android and iOS have never been built or
    /// run, and a scan of directories that are not there is a slow way to find
    /// nothing.
    #[test]
    fn a_platform_with_no_font_directories_is_not_guessed_at() {
        assert!(search_roots(&Probe::default()).is_empty());
    }

    /// A machine with no home directory — a service account, a container —
    /// still gets the system directories rather than nothing.
    #[test]
    fn the_system_directories_survive_a_machine_with_no_home() {
        let p = Probe {
            os: Some(Os::Unix),
            ..Default::default()
        };
        assert!(names(&search_roots(&p)).contains(&"/usr/share/fonts".to_string()));
    }

    #[test]
    fn only_formats_that_can_actually_be_drawn_with_are_offered() {
        for good in ["a.ttf", "a.OTF", "a.ttc", "a.otc"] {
            assert!(is_font_file(Path::new(good)), "{good}");
        }
        // Nothing here can parse these, and naming a face Umber cannot then set
        // text in is the control that lies.
        for bad in ["a.woff", "a.woff2", "a.pfb", "a.txt", "a"] {
            assert!(!is_font_file(Path::new(bad)), "{bad}");
        }
    }

    /// The library Umber starts with: the interface's own typeface, so a
    /// machine whose scan finds nothing can still set a caption.
    #[test]
    fn the_builtin_face_is_readable_and_files_itself_under_its_family() {
        let mut lib = FontLibrary::default();
        lib.add_builtin("archivo", TEST_FONT);
        assert_eq!(lib.families(), vec!["Archivo"], "{:?}", lib.faces());
        let face = lib.resolve("Archivo", "Regular").expect("a face");
        assert!(face.load().and_then(|d| d.font().map(|_| ())).is_some());
    }

    /// A variable font is one file carrying a whole family, and reading its
    /// default instance alone would offer "Regular" and hide the other eight
    /// weights behind a picker that looked like it was working.
    #[test]
    fn a_variable_fonts_named_instances_are_offered_as_styles() {
        let mut lib = FontLibrary::default();
        lib.add_builtin("archivo", TEST_FONT);
        let styles: Vec<&str> = lib.family("Archivo").iter().map(|f| f.label()).collect();
        assert!(styles.len() > 4, "only {styles:?}");
        assert!(styles.contains(&"Regular"), "{styles:?}");
        assert!(
            styles.iter().any(|s| s.eq_ignore_ascii_case("bold")),
            "{styles:?}"
        );
        // Exactly one style is the font's own default instance, and it is
        // recorded with **no** variations — which is what makes drawing with it
        // the exact identity rather than an instancing pass that happens to
        // change nothing. (Archivo's default is SemiBold, not Regular, which is
        // exactly why this is read off the axes rather than assumed.)
        let defaults = lib
            .family("Archivo")
            .into_iter()
            .filter(|f| f.variations.is_empty())
            .count();
        assert_eq!(defaults, 1, "{:?}", lib.faces());
        let bold = lib
            .family("Archivo")
            .into_iter()
            .find(|f| f.label().eq_ignore_ascii_case("bold"))
            .expect("a bold");
        assert!(
            bold.variations
                .iter()
                .any(|(t, v)| t == "wght" && *v > 600.0),
            "{:?}",
            bold.variations
        );
        assert_eq!(bold.weight, 700);
    }

    /// A preference records a family and a style by name, and the machine it is
    /// read back on may not have either. Every one of those readings has to
    /// land on a face, because the alternative is a Text panel that cannot draw
    /// anything until somebody notices why.
    #[test]
    fn a_face_that_is_not_on_this_machine_resolves_to_one_that_is() {
        let mut lib = FontLibrary::default();
        lib.add_builtin("archivo", TEST_FONT);
        assert!(lib.resolve("Helvetica Neue", "Bold").is_some());
        assert!(lib.resolve("Archivo", "A style it does not have").is_some());
    }

    /// One typeface spelled two ways is **one** row in the list, and that row
    /// reaches every one of its weights.
    ///
    /// Two files naming one family with different capitals is an ordinary thing
    /// to find on a real disk. They sort into one contiguous run under
    /// `insert`'s lowercased key, so a raw-string comparison in `families`
    /// started a second row part way through it — and `family` then handed each
    /// row only its own spelling, hiding half a typeface's weights behind a
    /// picker that looked complete.
    #[test]
    fn one_typeface_spelled_two_ways_is_one_family_with_all_its_weights() {
        let mut lib = FontLibrary::default();
        let face = |family: &str, style: &str, weight: u16| Face {
            family: family.to_string(),
            style: style.to_string(),
            weight,
            italic: false,
            source: Source::File {
                path: PathBuf::from(format!("{family}-{style}.ttf")),
                index: 0,
            },
            variations: Vec::new(),
        };
        lib.insert(face("Archivo", "Regular", 400));
        lib.insert(face("ARCHIVO", "Medium", 500));
        lib.insert(face("archivo", "Bold", 700));

        assert_eq!(lib.families().len(), 1, "{:?}", lib.families());
        let styles: Vec<&str> = lib
            .family(lib.families()[0])
            .iter()
            .map(|f| f.label())
            .collect();
        assert_eq!(styles, vec!["Regular", "Medium", "Bold"], "{styles:?}");
        // And every spelling still resolves, whichever one a preferences file
        // happens to have recorded.
        for name in ["Archivo", "ARCHIVO", "archivo", "ArChIvO"] {
            assert_eq!(
                lib.resolve(name, "bold").map(|f| f.weight),
                Some(700),
                "{name}"
            );
        }
    }

    /// The same family in two directories is one row in the list, not two — the
    /// ordinary case on Linux, where a family routinely sits in
    /// `/usr/share/fonts` and in the user's own directory as well.
    #[test]
    fn the_same_face_found_twice_is_listed_once() {
        let mut lib = FontLibrary::default();
        lib.add_builtin("archivo", TEST_FONT);
        let once = lib.faces().len();
        lib.add_builtin("archivo", TEST_FONT);
        assert_eq!(lib.faces().len(), once, "{:?}", lib.faces());
    }
}
