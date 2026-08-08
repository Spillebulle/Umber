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

/// Where [`FontLibrary::restyle`] cuts a family into its bold half and its
/// regular half.
///
/// 600 rather than the 700 CSS calls bold, and that is a reading of real font
/// libraries rather than of the specification. A family whose heaviest upright
/// is SemiBold at 600, or Black at 900 with no 700 anywhere at all, is entirely
/// ordinary. A threshold of exactly 700 would answer "this family has no bold"
/// for a family that plainly has one, and the only alternative to offering its
/// heavier face is **synthesising** a bold, which `restyle` refuses to do.
///
/// **It is a partition and not a reading of "is this text bold".** A nine-weight
/// family has four faces at or above it, and lighting a Bold control from
/// [`Face::is_bold`] would say the text was bold while it was SemiBold, with the
/// family's actual Bold two rows further down the same list — and would then take
/// a press as "make it regular", so no press would ever *reach* Bold from
/// SemiBold. [`FontLibrary::is_bold_anchor`] is the reading a control lights
/// from; this is only which side of the family a face is on.
pub const BOLD_THRESHOLD: u16 = 600;

/// The weight [`FontLibrary::restyle`] aims at when it is asked for bold.
///
/// `usWeightClass`'s own bold, and the target rather than the test: a family
/// carrying SemiBold, Bold, ExtraBold and Black hands back Bold, and one
/// carrying only SemiBold hands back that.
const BOLD_WEIGHT: u16 = 700;

/// The weight [`FontLibrary::restyle`] aims at when the bold comes off.
const REGULAR_WEIGHT: u16 = 400;

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

/// What [`FontLibrary::resolve`] had to change to be able to answer.
///
/// `resolve` is total by construction — a preference records names and the
/// machine it is read back on may have neither — so it can never *refuse*, and
/// something other than `resolve` has to be what says a substitution happened.
/// This is that reading, kept beside the rule it reads so the two cannot drift.
///
/// Which half was changed matters, because the two are different sentences: a
/// family that is not here at all is a font to go and install, where a family
/// that is here without the style asked for is a weight that was never in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Substitution {
    /// Nothing in the library carries that family, so this is some other
    /// typeface entirely.
    Family,
    /// The family is here and this style within it is not.
    Style,
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
    /// while the file it comes out of holds nine weights and every width.
    ///
    /// **Two rasterisers are in this binary and only one of them applies
    /// these**, which is worth being exact about because the sentence that used
    /// to sit here was not. `crate::text::set` hands the location to `skrifa`
    /// and `harfrust` and therefore genuinely draws the weight asked for —
    /// `a_named_instance_is_actually_drawn_at_its_own_weight` and
    /// `pressing_bold_actually_puts_a_heavier_mark_on_the_canvas` are what say
    /// so, both by rasterising and comparing rather than by reading a field.
    /// egui's own text goes through `ab_glyph`, which ignores variation axes and
    /// renders the default master whatever weight is asked for — **which is why
    /// the *interface* has no bold**, and it is the reason `cputext.rs` exists
    /// for the splash. That limit belongs to the panels, not to what the text
    /// tool puts on the canvas.
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

    /// Which half of its family this face is in, [`BOLD_THRESHOLD`]'s partition.
    ///
    /// Read off the weight and never off the style *name*. "Bold", "Gras",
    /// "Negrita", "Demi", "Heavy" and "Black" are all one answer here, and a
    /// name comparison would get every family not named in English wrong — the
    /// `name` table is localised and [`string`] takes the first record it finds.
    ///
    /// **This is not "the Bold control is on".** Four of Archivo's nine upright
    /// weights answer `true`, and only one of them is the family's bold; see
    /// [`BOLD_THRESHOLD`] and [`FontLibrary::is_bold_anchor`].
    pub fn is_bold(&self) -> bool {
        self.weight >= BOLD_THRESHOLD
    }
}

/// The most ordinary face of a set: upright, and nearest `REGULAR_WEIGHT`.
///
/// One statement of it, because [`FontLibrary::resolve`] falls back **twice** —
/// to the family without the style asked for, and then to the library without
/// either — and the second used to be `faces.first()`. That is the *lightest*
/// face of the alphabetically first family, because `FontLibrary::insert` sorts
/// on the weight: a family that had moved off the disk was therefore substituted
/// with Archivo **Thin**, so every caption in a document opened on another
/// machine came back as a hairline. It was found by looking at a picture of the
/// panel, which named the substitute out loud; nothing asserted it either way.
///
/// The weight is in the key after the distance so a tie is decided rather than
/// left to iteration order, and it decides for the lighter face, which is what
/// the sort order already produced.
fn most_ordinary<'a>(faces: impl Iterator<Item = &'a Face>) -> Option<&'a Face> {
    faces.min_by_key(|f| (f.italic, f.weight.abs_diff(REGULAR_WEIGHT), f.weight))
}

/// [`axis_agreement`]'s best answer: the two sit at the same place on every axis
/// an emphasis may not move.
const AXES_AGREE: u8 = 0;

/// One of the two faces does not state where it is, so there is nothing to
/// compare. Ranked between the other two so it can neither beat an agreement nor
/// lose to a disagreement — see [`axis_agreement`].
const AXES_UNKNOWN: u8 = 1;

/// They are at different places on an axis that was not asked to move.
const AXES_DISAGREE: u8 = 2;

/// How well `candidate` matches where `from` sits on every variable axis an
/// emphasis is **not** allowed to move. Lower is better; it is the first term of
/// [`FontLibrary::restyle`]'s sort key.
///
/// A modern family in one file is a **grid rather than a row**: a two-axis face
/// carries every weight at every width, so "the bold of Condensed Light" and
/// "the bold of Expanded Light" are both weight 700 and only one of them is the
/// answer. Without this, asking a condensed face for its bold hands back plain
/// Bold and loses the width somebody chose, silently and with nothing on screen
/// to say it happened.
///
/// `wght`, `ital` and `slnt` are skipped because those are exactly what is being
/// changed. Every other axis of `candidate` is looked up **by tag** in `from`
/// rather than walked in step: both faces usually come out of one `fvar` table
/// and [`read_faces`] records them in its order, so walking would agree, but two
/// files of one family (a roman and an italic) need not order their axes alike.
///
/// **Three answers rather than two, and the middle one is the whole of why.** A
/// variable font's own default instance carries **no variations at all** —
/// [`read_faces`] records it that way deliberately, because an empty list is
/// what tells `crate::text::set` there is nothing to instance — so a face with an
/// empty list has not said where it is on any axis. Read as agreement that cuts
/// both ways and one way is a *wrong face*: with the default master as a
/// candidate, "agrees with everything" made it tie with the correctly-widthed
/// face on every term and let the library's own order decide, so a condensed face
/// asked for its bold could be handed the wide default. `AXES_UNKNOWN` keeps it
/// behind a real agreement and ahead of a real disagreement, which is the only
/// ranking that is honest about not knowing. Where **`from`** is the default the
/// term goes uniform and the choice falls through to the weight rule, which is
/// what it was before this function existed: a gap, not a wrong answer, and the
/// honest cost of the empty-is-the-identity fast path. Closing that half means
/// recording the default's own axis values and giving `set` another way to spot
/// the identity.
///
/// Near-equality, and the tolerance is deliberately tiny: both sides are `f32`s
/// read off disk unmodified, so they agree bit for bit or they are at different
/// axis positions. A tolerance wide enough to be "forgiving" would merge the two
/// ends of a custom 0..1 axis such as Recursive's `CASL`.
fn axis_agreement(candidate: &Face, from: &Face) -> u8 {
    if candidate.variations.is_empty() || from.variations.is_empty() {
        return AXES_UNKNOWN;
    }
    for (tag, want) in &candidate.variations {
        if matches!(tag.as_str(), "wght" | "ital" | "slnt") {
            continue;
        }
        match from.variations.iter().find(|(t, _)| t == tag) {
            Some((_, have)) if (want - have).abs() < 1e-6 => {}
            Some(_) => return AXES_DISAGREE,
            None => return AXES_UNKNOWN,
        }
    }
    AXES_AGREE
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

    /// Put one face in the library, on [`Self::insert`]'s terms.
    ///
    /// The general form of [`Self::add_builtin`] and [`Self::add_file`], and it
    /// is `pub` for one reason: the states a style control can be refused in need
    /// families the shipped typeface cannot produce — one carrying a bold and no
    /// bold italic, one of a single heavy weight, an italic-only script face —
    /// and `textpanel`'s guard for those sentences lives in another crate.
    ///
    /// A [`Face`] is a name and a [`Source`], and `Source` is `pub`, so a caller
    /// **can** hand over one that names real bytes; the tests here do not, and
    /// this comment used to claim the function could not. What it does guarantee
    /// is [`Self::insert`]'s: the `(family, style)` name is unique and the family
    /// is not empty, so nothing added this way can break what
    /// [`Self::is_bold_anchor`] relies on.
    pub fn add_face(&mut self, face: Face) {
        self.insert(face);
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
    ///
    /// **What counts as a duplicate is the `(family, style)` name and nothing
    /// else**, which is stricter than the order key below and is what makes
    /// [`Self::exact`] a bijection. Two faces of one family sharing a style name
    /// at different weights is a malformed library rather than a hypothetical,
    /// and keeping both is worse than dropping one in three ways at once: the
    /// picker draws two rows with identical labels and highlights both, `exact`
    /// answers with whichever sorts first, and [`Self::restyle`] can therefore
    /// choose the heavier and hand back a *name* that resolves to the lighter —
    /// so the style mark lands on a face the model did not pick and reads its
    /// own state off a third one. A name is how a style is recorded, in a
    /// preferences file and in this panel, so a name that does not identify a
    /// face is the thing to refuse.
    ///
    /// **A face with no family name is refused too.** [`read_face`] cannot
    /// produce one — it returns `None` where the `name` table has no usable
    /// family — but [`Self::add_face`] is a public door, and one empty name
    /// becomes an empty row in the family picker *and* [`Self::resolve`]'s
    /// whole-library fallback for every family that is missing, which is the
    /// substitute every caption in a moved document would come back in.
    ///
    /// The cost is that the duplicate check is a linear scan per insert where the
    /// order key was a binary search, so a scan is quadratic in the faces it
    /// finds. Measured on a real Windows installation, 452 faces is about a
    /// hundred thousand comparisons and 0.65 ms, against 149 ms for the warm
    /// scan and 1.58 s cold in the same loop; a designer's collection is a few
    /// million comparisons. It is on a worker thread either way — see the module
    /// docs — and the figure to re-measure before changing this is that ratio,
    /// not the count.
    fn insert(&mut self, face: Face) {
        if face.family.trim().is_empty() {
            return;
        }
        if self.exact(&face.family, &face.style).is_some() {
            return;
        }
        // Sorted by family, then **weight**, then upright before italic — the
        // order the style list reads in, which is not the order the check above
        // dedupes by. A key that sorted on the name would list a family's
        // weights alphabetically.
        let key = |f: &Face| {
            (
                f.family.to_lowercase(),
                f.weight,
                f.italic,
                f.style.to_lowercase(),
            )
        };
        let k = key(&face);
        let at = self.faces.partition_point(|f| key(f) < k);
        self.faces.insert(at, face);
    }

    pub fn faces(&self) -> &[Face] {
        &self.faces
    }

    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }

    /// Every family name, in the order the list draws them, **without
    /// allocating**.
    ///
    /// This is the one the drawing path takes. A machine with several hundred
    /// families is the ordinary case, the Text panel's picker runs on every
    /// frame it is open, and a `Vec` of several hundred `&str` per frame to
    /// answer "how many match what has been typed" is exactly the cost the rule
    /// about the drawing path is written against.
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
    pub fn families_iter(&self) -> impl Iterator<Item = &str> {
        self.faces.iter().enumerate().filter_map(|(i, face)| {
            // The run's first face, which is the spelling that gets shown. `i`
            // rather than a carried "last": a `filter_map` closure that
            // remembered the previous name would be a second piece of state
            // saying the same thing as the sort order already does.
            let starts_a_run =
                i == 0 || !self.faces[i - 1].family.eq_ignore_ascii_case(&face.family);
            starts_a_run.then_some(face.family.as_str())
        })
    }

    /// [`Self::families_iter`], collected.
    ///
    /// Written in terms of it rather than beside it, so the two cannot come to
    /// disagree about where one typeface's run of spellings begins — which is
    /// the bug `one_typeface_spelled_two_ways_is_one_family_with_all_its_
    /// weights` exists for, and it would be invisible if only one of the pair
    /// were fixed.
    pub fn families(&self) -> Vec<&str> {
        self.families_iter().collect()
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
    /// then the most ordinary face in the whole library. The caller is what
    /// says a substitution happened; this only refuses when the library is
    /// empty.
    pub fn resolve(&self, family: &str, style: &str) -> Option<&Face> {
        if let Some(exact) = self.exact(family, style) {
            return Some(exact);
        }
        most_ordinary(
            self.faces
                .iter()
                .filter(|f| f.family.eq_ignore_ascii_case(family)),
        )
        .or_else(|| most_ordinary(self.faces.iter()))
    }

    /// The face this exact `(family, style)` pair names, and nothing else.
    ///
    /// [`Self::resolve`] is total on purpose and therefore cannot say "no";
    /// this is the half that can, and it is what anything reasoning about
    /// *this* face has to ask. `resolve` is written in terms of it, so the two
    /// cannot come to disagree about what an exact match is.
    ///
    /// Case-insensitively on both halves, for the reasons `resolve` gives: the
    /// family may be spelled several ways across one typeface's files, and the
    /// style comes back out of a preferences file somebody may have edited.
    pub fn exact(&self, family: &str, style: &str) -> Option<&Face> {
        self.faces
            .iter()
            .find(|f| f.family.eq_ignore_ascii_case(family) && f.style.eq_ignore_ascii_case(style))
    }

    /// The face of `family` that is this one with bold and italic as asked for.
    ///
    /// **`None` is an answer the interface draws**, and that is the whole point
    /// of the `Option`. A family with no italic on this machine has no italic,
    /// and the two ways of pretending otherwise are both refused here: **Umber
    /// never smears an outline sideways to make a bold and never shears one to
    /// make an oblique.** A fake bold is a blur at every size, a fake oblique
    /// bends letters a designer drew straight, and both are what a text engine
    /// does when it has given up — a family that carries a *real* bold puts the
    /// difference side by side on the same line. So the controls that ask for
    /// these are **disabled with a sentence** wherever this answers `None`; see
    /// `textpanel::emphasis`.
    ///
    /// Which face, when there is one: among the family's faces of the slant
    /// asked for and on the side of [`BOLD_THRESHOLD`] asked for, the one
    /// nearest a target weight. `axis_agreement` is ahead of the weight in the
    /// sort key, so the width and every other axis somebody chose survives; ties
    /// on both break to the lighter face, and a tie on *that* falls to the
    /// library's own order, which is `style` alphabetically within a weight.
    ///
    /// **What decides the target weight is which half of the pair moved.**
    /// Where the *slant* changed this is the italic control, so the weight is
    /// kept and Light asked for its italic gets Light Italic. Otherwise the
    /// weight is what moved, so the target is `BOLD_WEIGHT` or
    /// `REGULAR_WEIGHT` — and it has to be read that way round rather than off
    /// the face's own half: SemiBold is already in the bold half, so a rule that
    /// kept the weight whenever the half agreed would hand SemiBold straight
    /// back and the Bold control would do nothing. See
    /// [`Self::is_bold_anchor`] for the other half of that fix.
    ///
    /// **It is not an exact inverse, and that is deliberate.** Bold on and off
    /// again from Light lands on Regular rather than back on Light, because the
    /// alternative is a second piece of state — "the style this was before" —
    /// beside the style *name* that is the panel's one source of truth, and a
    /// second record of the same thing is what drifts. Every application with a
    /// Bold button behaves this way.
    pub fn restyle(&self, family: &str, style: &str, bold: bool, italic: bool) -> Option<&Face> {
        let from = self.exact(family, style);
        let want = match from {
            Some(f) if f.italic != italic => f.weight,
            _ if bold => BOLD_WEIGHT,
            _ => REGULAR_WEIGHT,
        };
        self.faces
            .iter()
            .filter(|f| {
                f.family.eq_ignore_ascii_case(family) && f.italic == italic && f.is_bold() == bold
            })
            .min_by_key(|f| {
                (
                    // Ahead of the weight, so a face agreeing with the one in
                    // hand about the width wins whatever the weights say. With
                    // no face in hand the term is uniform and decides nothing.
                    from.map_or(AXES_UNKNOWN, |c| axis_agreement(f, c)),
                    f.weight.abs_diff(want),
                    f.weight,
                )
            })
    }

    /// Whether any face of this family is here at all.
    ///
    /// What tells "that font is not on this machine" from "that font is here and
    /// the style recorded beside it is not". [`Self::exact`] answers about the
    /// **pair**, so reading its `None` as a missing family is how a mark came to
    /// tell somebody to install a font they already had — and `Substitution` has
    /// carried the distinction all along, because the panel draws a different
    /// sentence for each.
    pub fn has_family(&self, name: &str) -> bool {
        self.faces
            .iter()
            .any(|f| f.family.eq_ignore_ascii_case(name))
    }

    /// Whether [`Self::restyle`] would answer.
    ///
    /// What a control draws itself from, delegating to the plan it stands for —
    /// the arrangement `plan_reorder`/`can_reorder` already keeps, so a control
    /// cannot light up promising something the model will then decline.
    ///
    /// It answers `true` for a pair a face already satisfies, because `restyle`
    /// would hand that face back. No caller asks that: the panel's marks always
    /// flip one half. A keystroke route that passed the *current* pair would get
    /// a lit control that does nothing, and would want its own guard.
    pub fn can_restyle(&self, family: &str, style: &str, bold: bool, italic: bool) -> bool {
        self.restyle(family, style, bold, italic).is_some()
    }

    /// Whether the family's own bold, at this face's slant, **is** this face.
    ///
    /// **This is what a Bold control lights from, and [`Face::is_bold`] is
    /// not.** `is_bold` is [`BOLD_THRESHOLD`]'s partition, and Archivo has four
    /// upright faces on the bold side of it; lighting from that said the text was
    /// bold while it was SemiBold, and — because a lit control asks for the
    /// regular weight — meant no press ever reached Bold from SemiBold,
    /// ExtraBold or Black. Those three lost two or three weights on one click,
    /// silently.
    ///
    /// Asking the *anchor* instead — is the bold you would be given the one you
    /// already have — is lit for exactly one face of each slant and reachable in
    /// one press from every other. So SemiBold reads as not bold, which is a
    /// small thing to say about a heavy face and is what every application with
    /// a style menu beside a `B` says about it.
    ///
    /// Written in terms of [`Self::restyle`] rather than beside it, so the mark
    /// and the press cannot disagree about which face is the bold: identity, not
    /// equality, because two faces of one family can only be told apart reliably
    /// by *being* the same entry.
    pub fn is_bold_anchor(&self, family: &str, style: &str) -> bool {
        let Some(face) = self.exact(family, style) else {
            return false;
        };
        self.restyle(family, style, true, face.italic)
            .is_some_and(|bold| std::ptr::eq(bold, face))
    }

    /// Whether the family carries any face on the bold side of
    /// [`BOLD_THRESHOLD`], at either slant.
    ///
    /// Not what decides whether a control may be pressed — `can_restyle` is —
    /// but what tells the two refusals apart: a family with no bold anywhere is
    /// a font to go and install, where one with a bold and no *bold italic* is a
    /// combination it never had. Naming the wrong one sends somebody looking for
    /// a file they already have.
    pub fn offers_bold(&self, family: &str) -> bool {
        self.faces
            .iter()
            .any(|f| f.family.eq_ignore_ascii_case(family) && f.is_bold())
    }

    /// Whether the family carries any italic, at any weight. See
    /// [`Self::offers_bold`] for why this is asked separately from
    /// [`Self::can_restyle`].
    pub fn offers_italic(&self, family: &str) -> bool {
        self.faces
            .iter()
            .any(|f| f.family.eq_ignore_ascii_case(family) && f.italic)
    }

    /// Whether [`Self::resolve`] answered with something other than what was
    /// asked for, and which half of the name it had to change.
    ///
    /// `None` where the pair named a real face, which is the ordinary case and
    /// the one the interface stays quiet about. It reads `resolve`'s own answer
    /// rather than re-deciding, so a caller cannot be told there was no
    /// substitution and then be handed a substitute.
    ///
    /// Case-insensitively on both halves, exactly as `resolve` matches: a
    /// preferences file spells a family however whoever edited it spelled it,
    /// and reporting "Archivo is not here, using ARCHIVO" would be a notice
    /// about nothing.
    /// **The face comes back with the answer**, so the caller naming it in a
    /// sentence does not have to `resolve` a second time. That is one linear
    /// scan per frame rather than two on a machine with a few thousand faces,
    /// and it removes the only way the sentence could name a third thing.
    pub fn substituted(&self, family: &str, style: &str) -> Option<(Substitution, &Face)> {
        let face = self.resolve(family, style)?;
        if !face.family.eq_ignore_ascii_case(family) {
            Some((Substitution::Family, face))
        } else if !face.style.eq_ignore_ascii_case(style) {
            Some((Substitution::Style, face))
        } else {
            None
        }
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

    /// `resolve` is total, so something else has to say it substituted — and
    /// the panel names the face it is *actually* setting in only because this
    /// answers.
    ///
    /// A missing family and a missing style are told apart, because they are
    /// different sentences: one is a typeface to go and install, the other is a
    /// weight that was never in the one you have.
    #[test]
    fn a_face_that_had_to_be_substituted_says_which_half_of_the_name_moved() {
        let mut lib = FontLibrary::default();
        lib.add_builtin("archivo", TEST_FONT);

        // The ordinary case is silence.
        assert!(lib.substituted("Archivo", "Regular").is_none());
        // And spelling is not a substitution: `resolve` matches case
        // insensitively, so a preferences file written in another capital must
        // not raise a notice about nothing.
        assert!(lib.substituted("ARCHIVO", "regular").is_none());

        assert_eq!(
            lib.substituted("Helvetica Neue", "Bold").map(|(w, _)| w),
            Some(Substitution::Family)
        );
        assert_eq!(
            lib.substituted("Archivo", "Ultra Condensed Black Italic")
                .map(|(w, _)| w),
            Some(Substitution::Style)
        );
        // And it hands back the face it read the answer off, so a caller
        // naming both halves cannot name a third thing by resolving again.
        let (_, face) = lib
            .substituted("Helvetica Neue", "Bold")
            .expect("a substitute");
        assert!(std::ptr::eq(
            face,
            lib.resolve("Helvetica Neue", "Bold").unwrap()
        ));
        // Nothing to substitute *with* is not a substitution either — it is the
        // one case `resolve` refuses, and the panel has nothing to draw.
        assert!(
            FontLibrary::default()
                .substituted("Archivo", "Regular")
                .is_none()
        );
    }

    /// The borrowing iterator and the collecting one answer the same thing.
    ///
    /// They are one implementation now, so this reads as a tautology; it is
    /// here because they were two, and the case that told them apart is the
    /// case above — a run of spellings, where a walk that carried the previous
    /// name and one that indexes backwards are easy to write differently. The
    /// panel takes the iterator on every frame and the tests take the `Vec`,
    /// so a difference would show up nowhere either of them looks.
    #[test]
    fn the_borrowing_family_list_says_what_the_collected_one_does() {
        let mut lib = FontLibrary::default();
        lib.add_builtin("archivo", TEST_FONT);
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
        lib.insert(face("Zapfino", "Regular", 400));
        lib.insert(face("ZAPFINO", "Bold", 700));
        lib.insert(face("Bodoni", "Regular", 400));

        let collected = lib.families();
        let walked: Vec<&str> = lib.families_iter().collect();
        assert_eq!(walked, collected, "{:?}", lib.faces());
        assert_eq!(walked.len(), 3, "{walked:?}");
        // Empty is the answer for an empty library rather than one blank row.
        assert_eq!(FontLibrary::default().families_iter().count(), 0);
    }

    /// A face built by hand, for the cases a real font file cannot produce.
    fn made(family: &str, style: &str, weight: u16, italic: bool) -> Face {
        Face {
            family: family.to_string(),
            style: style.to_string(),
            weight,
            italic,
            source: Source::File {
                path: PathBuf::from(format!("{family}-{style}.ttf")),
                index: 0,
            },
            variations: Vec::new(),
        }
    }

    /// A family that is not here is substituted with something **ordinary**, not
    /// with whatever sorted first.
    ///
    /// The last fallback used to be `faces.first()`, and `insert` sorts on the
    /// weight, so the answer was the lightest face of the alphabetically first
    /// family: a document whose fonts had moved came back set in Archivo Thin,
    /// as a hairline. Noticed by looking at a picture of the panel, which names
    /// the substitute in a sentence.
    #[test]
    fn a_substitute_for_a_missing_family_is_an_ordinary_weight() {
        let mut lib = FontLibrary::default();
        lib.add_builtin("archivo", TEST_FONT);
        let face = lib
            .resolve("A Foundry Face Nobody Has", "Regular")
            .expect("a substitute");
        assert!(!face.italic, "{face:?}");
        assert!(
            (350..=550).contains(&face.weight),
            "substituted a weight nobody would set a caption in: {face:?}"
        );

        // The same rule where the *style* is what is missing, which is the
        // fallback one step earlier and always behaved this way.
        let face = lib
            .resolve("Archivo", "Ultra Condensed Black Italic")
            .expect("a substitute");
        assert!(
            !face.italic && (350..=550).contains(&face.weight),
            "{face:?}"
        );
    }

    /// `exact` refuses where `resolve` substitutes, which is the whole reason it
    /// exists: everything reasoning about *this* face needs the half that can
    /// say no.
    #[test]
    fn the_exact_lookup_refuses_what_resolve_substitutes_for() {
        let mut lib = FontLibrary::default();
        lib.add_builtin("archivo", TEST_FONT);
        assert!(lib.exact("Archivo", "Regular").is_some());
        // Both spellings, because a preferences file records whatever the
        // person who edited it typed.
        assert!(lib.exact("ARCHIVO", "regular").is_some());
        assert!(
            lib.exact("Archivo", "Ultra Condensed Black Italic")
                .is_none()
        );
        assert!(lib.exact("Helvetica Neue", "Regular").is_none());
        // And `resolve` still answers for both of those, which is what makes
        // the pair necessary rather than redundant.
        assert!(lib.resolve("Helvetica Neue", "Regular").is_some());
    }

    /// **Every weight of the family reaches its bold in one press, and exactly
    /// one weight per slant reads as bold.**
    ///
    /// This is the case a threshold read as "is this text bold" gets wrong, and
    /// the shipped font is that case: four of Archivo's nine upright faces are on
    /// the bold side of [`BOLD_THRESHOLD`]. Lighting the control from
    /// [`Face::is_bold`] said SemiBold was bold, and a lit control asks for the
    /// *regular* weight — so pressing Bold on SemiBold, ExtraBold or Black went
    /// down two or three weights and the family's own Bold could not be reached
    /// by any sequence of presses at all.
    #[test]
    fn every_weight_of_a_family_reaches_its_bold_in_one_press() {
        let mut lib = FontLibrary::default();
        lib.add_builtin("archivo", TEST_FONT);

        let styles: Vec<String> = lib
            .family("Archivo")
            .iter()
            .map(|f| f.style.clone())
            .collect();
        // The premise. With one face on the bold side this test guards nothing,
        // and the defect it exists for could not happen.
        let heavy = lib
            .family("Archivo")
            .iter()
            .filter(|f| f.is_bold() && !f.italic)
            .count();
        assert!(heavy > 1, "only {heavy} upright faces are on the bold side");

        // Exactly one upright face is the anchor, so exactly one row of the
        // style list draws the mark lit.
        let anchors: Vec<&String> = styles
            .iter()
            .filter(|s| lib.is_bold_anchor("Archivo", s))
            .collect();
        assert_eq!(anchors.len(), 1, "{anchors:?}");
        let anchor = anchors[0].clone();

        // And every other weight reaches it in a single press, including the
        // ones heavier than it.
        for style in &styles {
            if *style == anchor {
                continue;
            }
            let face = lib.exact("Archivo", style).expect("a face");
            let landed = lib
                .restyle("Archivo", style, true, face.italic)
                .expect("a bold");
            assert_eq!(
                landed.style, anchor,
                "{style} was not offered the family's own bold"
            );
        }

        // Pressing it while it is lit goes to something lighter, never back to
        // itself: a mark that is on and does nothing is the worst of the three.
        let anchor_weight = lib.exact("Archivo", &anchor).expect("a face").weight;
        let back = lib
            .restyle("Archivo", &anchor, false, false)
            .expect("something lighter");
        assert!(back.weight < anchor_weight, "{back:?}");
        assert!(!back.is_bold());
    }

    /// Bold is a **real face of the family** or it is nothing.
    ///
    /// Archivo carries nine weights in one file, so its bold is reachable and
    /// heavier; it carries no italic at all, so the italic is `None` — and
    /// `None` is the answer the panel draws as a disabled control rather than
    /// shearing the upright outlines into a fake oblique.
    #[test]
    fn bold_is_a_real_face_of_the_family_and_a_missing_italic_is_refused() {
        let mut lib = FontLibrary::default();
        lib.add_builtin("archivo", TEST_FONT);

        let bold = lib
            .restyle("Archivo", "Regular", true, false)
            .expect("a bold");
        assert!(bold.is_bold(), "{bold:?}");
        assert!(bold.weight >= BOLD_THRESHOLD);
        assert_eq!(bold.family, "Archivo");
        // It is one of the styles the picker lists, never a name invented here.
        assert!(
            lib.family("Archivo")
                .iter()
                .any(|f| f.style == bold.style && f.weight == bold.weight),
            "{} is not a style of the family",
            bold.style
        );

        // The shipped file is upright only. Nothing here fabricates a slant.
        assert!(
            lib.restyle("Archivo", "Regular", false, true).is_none(),
            "an italic was invented for a family that has none"
        );
        assert!(!lib.can_restyle("Archivo", "Regular", false, true));

        // And a family that is not here at all offers neither, rather than
        // reaching into whatever `resolve` would have substituted.
        assert!(!lib.can_restyle("A Foundry Face Nobody Has", "Regular", true, false));
        assert!(!lib.can_restyle("A Foundry Face Nobody Has", "Regular", false, true));
    }

    /// The bold coming off lands on the family's regular weight, and the italic
    /// going on keeps the weight it was asked from. One rule, two controls.
    #[test]
    fn taking_the_bold_off_finds_regular_and_the_italic_keeps_its_weight() {
        let mut lib = FontLibrary::default();
        for (style, weight, italic) in [
            ("Light", 300, false),
            ("Regular", 400, false),
            ("Bold", 700, false),
            ("Light Italic", 300, true),
            ("Italic", 400, true),
            ("Bold Italic", 700, true),
        ] {
            lib.insert(made("Foo", style, weight, italic));
        }

        // Bold on and off moves the weight to the family's own two anchors.
        assert_eq!(
            lib.restyle("Foo", "Regular", true, false)
                .map(|f| &*f.style),
            Some("Bold")
        );
        assert_eq!(
            lib.restyle("Foo", "Bold", false, false).map(|f| &*f.style),
            Some("Regular")
        );
        // Italic keeps it. This is the case that would be wrong under a single
        // "always aim at 400" rule: Light Italic, not Italic.
        assert_eq!(
            lib.restyle("Foo", "Light", false, true).map(|f| &*f.style),
            Some("Light Italic")
        );
        assert_eq!(
            lib.restyle("Foo", "Bold", true, true).map(|f| &*f.style),
            Some("Bold Italic")
        );
        assert_eq!(
            lib.restyle("Foo", "Bold Italic", true, false)
                .map(|f| &*f.style),
            Some("Bold")
        );

        // Not an exact inverse, and said out loud rather than left to be found:
        // Light bolded and un-bolded is Regular. See `restyle`.
        let there = lib.restyle("Foo", "Light", true, false).expect("a bold");
        assert_eq!(
            lib.restyle("Foo", &there.style, false, false)
                .map(|f| &*f.style),
            Some("Regular")
        );
    }

    /// A family whose heaviest face is SemiBold has a bold, and it is the
    /// SemiBold. [`BOLD_THRESHOLD`] is 600 for exactly this: at 700 the answer
    /// would be "no bold" for a family that plainly has one, and the only other
    /// way to produce one is to fake it.
    #[test]
    fn a_family_whose_heaviest_is_semibold_still_has_a_bold() {
        let mut lib = FontLibrary::default();
        lib.insert(made("Thin Air", "Regular", 400, false));
        lib.insert(made("Thin Air", "SemiBold", 600, false));
        assert_eq!(
            lib.restyle("Thin Air", "Regular", true, false)
                .map(|f| &*f.style),
            Some("SemiBold")
        );

        // Bold at 700 beats SemiBold at 600 where both are there, because the
        // threshold is the test and 700 is the target.
        lib.insert(made("Thin Air", "Bold", 700, false));
        assert_eq!(
            lib.restyle("Thin Air", "Regular", true, false)
                .map(|f| &*f.style),
            Some("Bold")
        );

        // A family of one weight offers nothing at all, in either direction.
        let mut alone = FontLibrary::default();
        alone.insert(made("Zapfino", "Regular", 400, false));
        assert!(!alone.can_restyle("Zapfino", "Regular", true, false));
        assert!(!alone.can_restyle("Zapfino", "Regular", false, true));
        // And a family of nothing *but* bold has no way back, which is a
        // disabled control rather than a lighter face fabricated for it.
        let mut heavy = FontLibrary::default();
        heavy.insert(made("Slab Only", "Black", 900, false));
        assert!(!heavy.can_restyle("Slab Only", "Black", false, false));
    }

    /// Bolding a condensed face keeps it condensed.
    ///
    /// A variable family is a grid: every width carries every weight, so several
    /// faces are weight 700 and only one of them is the bold *of this one*.
    /// Without [`same_other_axes`] the answer was whichever sorted first, which
    /// silently threw away the width somebody had chosen.
    #[test]
    fn bolding_a_condensed_face_keeps_the_width_it_was_asked_from() {
        let mut lib = FontLibrary::default();
        let grid = |style: &str, weight: u16, wdth: f32| Face {
            family: "Grid".to_string(),
            style: style.to_string(),
            weight,
            italic: false,
            source: Source::File {
                path: PathBuf::from("Grid[wdth,wght].ttf"),
                index: 0,
            },
            variations: vec![
                ("wdth".to_string(), wdth),
                ("wght".to_string(), weight as f32),
            ],
        };
        for (style, weight, wdth) in [
            ("Light", 300, 100.0),
            ("Regular", 400, 100.0),
            ("Bold", 700, 100.0),
            ("Condensed Light", 300, 75.0),
            ("Condensed Regular", 400, 75.0),
            ("Condensed Bold", 700, 75.0),
        ] {
            lib.insert(grid(style, weight, wdth));
        }
        assert_eq!(
            lib.restyle("Grid", "Condensed Regular", true, false)
                .map(|f| &*f.style),
            Some("Condensed Bold")
        );
        assert_eq!(
            lib.restyle("Grid", "Regular", true, false)
                .map(|f| &*f.style),
            Some("Bold")
        );
        assert_eq!(
            lib.restyle("Grid", "Condensed Bold", false, false)
                .map(|f| &*f.style),
            Some("Condensed Regular")
        );
    }

    /// **A face that states no axis position must not be read as agreeing with
    /// every width**, and this is the case that guards the rule rather than the
    /// case that happens to pass under it.
    ///
    /// `read_faces` records a variable font's own default instance with an empty
    /// `variations` list, deliberately, because that is what tells `text::set`
    /// there is nothing to instance. Exactly one face per file is like that, and
    /// it is a *candidate* like any other. Under a two-way comparison it agreed
    /// with everything, tied the correctly-widthed face on every term of the sort
    /// key, and let the library's own alphabetical order decide — so a condensed
    /// face asked for its bold could be handed the wide default master. The
    /// sibling case, where the *face in hand* is the default, is the documented
    /// gap and is checked here too: the preference goes uniform and the weight
    /// rule decides, which is a plainer answer rather than a wrong one.
    #[test]
    fn a_face_that_states_no_axis_position_does_not_claim_every_width() {
        let mut lib = FontLibrary::default();
        let one = |style: &str, weight: u16, wdth: Option<f32>| Face {
            family: "Grid".to_string(),
            style: style.to_string(),
            weight,
            italic: false,
            source: Source::File {
                path: PathBuf::from("Grid[wdth,wght].ttf"),
                index: 0,
            },
            variations: match wdth {
                Some(wdth) => vec![
                    ("wdth".to_string(), wdth),
                    ("wght".to_string(), weight as f32),
                ],
                // The file's default master, as `read_faces` records it.
                None => Vec::new(),
            },
        };
        lib.insert(one("Condensed Regular", 400, Some(75.0)));
        lib.insert(one("Condensed Bold", 700, Some(75.0)));
        // The default master is a bold, and sorts *before* "Condensed Bold"
        // alphabetically at the same weight — which is what made the old
        // comparison hand it back.
        lib.insert(one("Bold", 700, None));

        assert_eq!(
            lib.restyle("Grid", "Condensed Regular", true, false)
                .map(|f| &*f.style),
            Some("Condensed Bold"),
            "a face stating no width was taken to agree with this one"
        );

        // And the other half, which is the documented gap: with the default
        // master *in hand* nothing is known about its width, so the weight rule
        // decides and either bold is an honest answer.
        lib.insert(one("Condensed Light", 300, Some(75.0)));
        assert!(
            lib.restyle("Grid", "Bold", false, false).is_some(),
            "a default master in hand should still reach a lighter face"
        );
    }

    /// A face with no family name never enters the library.
    ///
    /// `read_face` cannot make one, but `add_face` is a public door, and one
    /// empty name is an empty row in the family picker *and* `resolve`'s
    /// whole-library fallback for every family that is missing — so every caption
    /// in a moved document would come back set in nothing.
    #[test]
    fn a_face_with_no_family_name_is_refused() {
        let mut lib = FontLibrary::default();
        lib.add_builtin("archivo", TEST_FONT);
        let before = lib.faces().len();
        for family in ["", "   "] {
            lib.add_face(made(family, "Regular", 400, false));
        }
        assert_eq!(lib.faces().len(), before, "{:?}", lib.families());
        assert_eq!(lib.families(), vec!["Archivo"]);
    }

    /// **A style name identifies a face**, so two faces of one family that share
    /// one cannot both be in the library.
    ///
    /// The order key tells them apart by weight, so keeping both compiles and
    /// sorts fine, and then costs three things at once: the picker draws two rows
    /// with identical labels and highlights both, `exact` answers with whichever
    /// sorts first, and `restyle` can pick the heavier and hand back a *name*
    /// that resolves to the lighter — so the style mark lands on a face the model
    /// did not choose and reads its own state off a third. A name is what a
    /// preference records and what the panel holds, so a name that does not
    /// identify a face is the thing to refuse.
    #[test]
    fn a_style_name_identifies_one_face_of_its_family() {
        let mut lib = FontLibrary::default();
        lib.insert(made("Foo", "Regular", 400, false));
        // Same family, same style name, a different weight: refused, first wins.
        lib.insert(made("Foo", "Regular", 500, false));
        // And a different spelling of both is still the same name.
        lib.insert(made("FOO", "REGULAR", 900, false));
        assert_eq!(lib.family("Foo").len(), 1, "{:?}", lib.faces());
        assert_eq!(lib.exact("Foo", "Regular").map(|f| f.weight), Some(400));

        // A different name at the same weight is a different face and is kept,
        // and a different family sharing a style name is untouched.
        lib.insert(made("Foo", "Book", 400, false));
        lib.insert(made("Bar", "Regular", 400, false));
        assert_eq!(lib.family("Foo").len(), 2, "{:?}", lib.faces());
        assert_eq!(lib.family("Bar").len(), 1);

        // Whatever the library holds, every style name it lists resolves back to
        // the face it came off. That is the property the whole rule exists for,
        // and `restyle` relies on it to be able to answer with a name.
        for face in lib.faces() {
            let back = lib.exact(&face.family, &face.style).expect("a face");
            assert!(
                std::ptr::eq(back, face),
                "{:?} does not resolve to itself",
                face
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
