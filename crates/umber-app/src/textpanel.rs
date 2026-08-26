//! The Text module: set a line of text and place it on the canvas.
//!
//! `umber_core::text` is the model — shaping, layout, rasterisation — and
//! `umber_core::fonts` is where the faces come from. Neither draws anything.
//! This module holds what the artist has typed, finds the faces on a worker
//! thread, and paints the panel. Same division `dock.rs` keeps against
//! `panels.rs`.
//!
//! # Why the typing happens in a panel and not on the canvas
//!
//! Because a caret on the canvas is a much larger feature than it looks, and
//! this one is useful without it. Key dispatch happens at the **winit** level,
//! before egui sees a keystroke, so an on-canvas caret needs `shortcuts`
//! suspended for it, characters taken from `KeyEvent::text` rather than the
//! physical key, `Enter` and `Escape` prised away from the float commands they
//! already mean — and, the one that decides it, **an IME**. Umber never calls
//! `set_ime_allowed` and never handles `WindowEvent::Ime`, so a canvas caret
//! could not type Chinese, Japanese or Korean at all, and nobody working on
//! Umber can test that it does once it is added.
//!
//! A real `egui::TextEdit` gets all of that for nothing: the caret, the
//! selection, the IME and — since the clipboard feature went on for `sysclip` —
//! Ctrl+C, Ctrl+X and Ctrl+V out to the rest of the machine. Pasting a
//! paragraph in from somewhere else is most of what somebody wants a text panel
//! for, and here it already works.
//!
//! # Placing is a paste
//!
//! [`crate::app::UmberApp::place_text`] turns the coverage into a `Clip` and
//! hands it to the same two calls `paste` uses — `Clip::place` and
//! `begin_float` — then switches to the transform tool. So the box has handles,
//! Escape abandons it, a click outside puts it down, the undo entry is a
//! `Transform` like a paste's, and the preview is byte for byte what commits.
//! Nothing new reaches the GPU and nothing new reaches the file.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};

use egui::{Sense, Ui, vec2};

use umber_core::Background;
use umber_core::fonts::{self, Face, FontLibrary, Substitution};
use umber_core::text::{self, Align, TextBlock, TextError};

use crate::controls;
use crate::editor::Editor;
use crate::icons::Icon;
use crate::theme::{Palette, metrics, text as texttokens};
use crate::ui::UiActions;
use crate::widgets::{self, DropdownWidth, NumberRow};

/// What the built-in face is registered as. A name rather than a path, because
/// it is the thing a preference would record and it must not be a file that is
/// not there.
pub const BUILTIN: &str = "archivo";

/// The em size the panel's preview is rasterised at.
///
/// Fixed rather than fitted, and the picture is scaled to the panel afterwards.
/// Fitting means rasterising once to measure and again to draw, on every
/// keystroke, and the second pass buys sharpness in a thumbnail nobody paints
/// with — what the artist is checking here is the face, the weight and the line
/// breaks, all of which survive a scale.
const PREVIEW_EM: f32 = 26.0;

/// The tallest the preview may be drawn, in points. Past this it is scaled
/// down: a panel is not where somebody reads six lines of a poem.
const PREVIEW_MAX_HEIGHT: f32 = 110.0;

/// Every face Umber can set text in, and how far along finding them is.
///
/// The built-in face is there from the first frame, so the panel is usable
/// before the scan lands and a machine whose scan finds nothing is still usable
/// afterwards. See `fonts`'s module docs for what the scan costs and why it is
/// therefore not on the drawing path.
pub struct Fonts {
    library: FontLibrary,
    /// The worker, while one is running. `None` before it starts and after it
    /// has landed.
    pending: Option<Receiver<FontLibrary>>,
    /// Whether a scan has been asked for. Separate from `pending` so a scan
    /// that finished is not started again every frame.
    started: bool,
    /// Bumped whenever `library` is replaced.
    ///
    /// **What everything cached off a face has to be keyed by.** The library is
    /// swapped wholesale when a scan lands and again when the folder
    /// preference changes, and the caches beside it hold a `FontData` and a
    /// picture resolved out of the *old* one — same family name, same style
    /// name, different file. Without this the panel goes on drawing a preview,
    /// a size and a missing-glyph notice made with a face `resolve` no longer
    /// answers with, while Place uses the new one; the two disagree silently
    /// until something else the key hashes happens to move.
    generation: u64,
    /// How many **families** the library holds, as the string the dropdown
    /// draws beside the family name.
    ///
    /// Families and not faces, and that is a fix rather than a detail. The
    /// figure used to be `faces().len()` while the *filtered* figure beside it
    /// counted families, so typing one character into the search field took the
    /// trigger from 45 to 1 on a library holding one typeface and put it back
    /// on backspace. Two quantities in one readout is the control that lies,
    /// and the menu under it lists families, so families is the honest one.
    ///
    /// Kept rather than formatted, because the dropdown draws it on every frame
    /// the panel is open and the figure changes twice in a session.
    families: String,
}

impl Default for Fonts {
    fn default() -> Self {
        let mut library = FontLibrary::default();
        library.add_builtin(BUILTIN, crate::cputext::ARCHIVO);
        let families = library.families_iter().count().to_string();
        Self {
            library,
            pending: None,
            started: false,
            generation: 0,
            families,
        }
    }
}

impl Fonts {
    pub fn library(&self) -> &FontLibrary {
        &self.library
    }

    /// Hold the library at the built-in face, so [`Self::start`] never scans.
    ///
    /// For `docshot`, and the reason `brushlib::stage_library` exists: the shot
    /// is committed, and a picture whose face count is the number of fonts
    /// installed on a contributor's machine is a picture of that machine. The
    /// scan lands on a *later* frame and `docshot` draws one, so the count was
    /// already the built-in's own nine styles — but that is a race won rather
    /// than a guarantee, and this makes it the second. It also stops the tool
    /// spawning a several-hundred-file thread it has no use for.
    pub fn hold_at_builtin(&mut self) {
        self.started = true;
    }

    pub fn scanning(&self) -> bool {
        self.pending.is_some()
    }

    /// How many families there are, as the dropdown draws it. See the field.
    pub fn family_count(&self) -> &str {
        &self.families
    }

    /// Which library this is. See the field.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Take a new library and tell everything cached off the old one.
    fn adopt(&mut self, library: FontLibrary) {
        self.families = library.families_iter().count().to_string();
        self.library = library;
        self.generation += 1;
    }

    /// Look for the machine's fonts, once, on a worker thread.
    ///
    /// On a thread because a scan is several hundred file reads — see `fonts` —
    /// and this is called from a panel body, which is the drawing path. The
    /// panel asks for a repaint while one is in flight rather than the thread
    /// waking the loop: it is bounded, it ends, and it is only ever running
    /// while the Text module is on screen. The update check's `EventLoopProxy`
    /// wake exists because *that* answer may arrive with nothing on screen
    /// waiting for it.
    pub fn start(&mut self, folder: &Option<PathBuf>) {
        if self.started {
            return;
        }
        self.started = true;
        // Cloned here and not by the caller: this runs on every frame the panel
        // is open and spawns on one of them.
        let folder = folder.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("umber-fonts".to_string())
            .spawn(move || {
                let mut library = FontLibrary::default();
                library.add_builtin(BUILTIN, crate::cputext::ARCHIVO);
                // The user's own folder **first**, so their explicit choice
                // beats a copy of the same family in `/usr/share/fonts`:
                // `FontLibrary::insert` keeps the first of a duplicate, and
                // somebody who pointed Umber at a directory meant that
                // directory.
                let mut roots = Vec::new();
                roots.extend(folder);
                roots.extend(fonts::search_roots(&fonts::Probe::here()));
                library.scan(&roots);
                let _ = tx.send(library);
            })
            .is_ok();
        if spawned {
            self.pending = Some(rx);
        } else {
            // A machine that will not give us a thread keeps the built-in face
            // and says nothing. There is no useful second attempt, and a
            // painting application must not refuse to draw a panel over it.
            log::warn!("could not start the font scan; only the built-in face is available");
        }
    }

    /// Take the scan's answer if it has arrived. True when it just did.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = self.pending.as_ref() else {
            return false;
        };
        match rx.try_recv() {
            Ok(library) => {
                log::info!(
                    "found {} faces in {} font files ({} unreadable)",
                    library.faces().len(),
                    library.scanned,
                    library.unreadable
                );
                self.adopt(library);
                self.pending = None;
                true
            }
            Err(TryRecvError::Empty) => false,
            // The worker died. The built-in face stands.
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                false
            }
        }
    }

    /// Throw the scan away so the next open of the panel looks again.
    ///
    /// What the folder preference calls when it changes: a library that still
    /// held the old folder's faces would offer faces the artist has just
    /// pointed Umber away from.
    pub fn forget(&mut self) {
        self.pending = None;
        self.started = false;
        let mut library = FontLibrary::default();
        library.add_builtin(BUILTIN, crate::cputext::ARCHIVO);
        self.adopt(library);
    }
}

/// What the artist has typed, and which face it is in.
///
/// Above the `--- documents ---` line in [`Editor`], and deliberately: a block
/// of text being composed belongs to the person rather than to the picture, in
/// the same way the brush in hand and the clipboard do. Switching tabs and
/// placing the same caption on a second document is the ordinary thing to want.
pub struct TextState {
    pub block: TextBlock,
    /// The family and style **by name**, never a `Face`. A `Face` holds a path,
    /// and the scan can replace the whole library underneath this — see
    /// `FontLibrary::resolve`, which is total for exactly this reason.
    pub family: String,
    pub style: String,
    pub fonts: Fonts,
    /// Filter for the family list. Several hundred families is not a list
    /// anybody scrolls.
    ///
    /// It is a field on the **panel**, not a widget inside the dropdown's menu,
    /// and that is not a layout preference. `widgets::dropdown` opens its menu
    /// with `egui::Popup::menu`, whose default close behaviour is
    /// `CloseOnClick` — *any* click, inside the popup included, which is
    /// correct for the `selectable_label`s every other call site puts in there
    /// and fatal for a text field: the click that would focus it is the click
    /// that shuts the menu, so the field could never be typed into at all.
    /// Above the trigger it is an ordinary widget, it stays put while the list
    /// is scrolled, and the trigger says how many families the filter is
    /// leaving.
    pub search: String,
    /// The bytes of the face in hand, and what they were resolved from.
    ///
    /// **A font file is read whole**, and a CJK collection is sixteen
    /// megabytes; the preview is rebuilt on every keystroke, so re-reading it
    /// there would put a disk read and a full parse between somebody and each
    /// character they type. Keyed by the family, the style *and*
    /// `Fonts::generation`, because the library is replaced wholesale when a
    /// scan lands and the same two names then mean a different file.
    loaded: Option<(String, String, u64, umber_core::fonts::FontData)>,
    /// The panel's picture of the block, and everything it costs to work out.
    ///
    /// Keyed by everything that changes the picture **including the colour**,
    /// so a preview cannot outlive the thing it is a preview of. Single
    /// consumer — the panel body, drawn once per pass — which is the property
    /// that makes one cache entry safe; see `brushlib::tip_preview` and the
    /// rule in CLAUDE.md about a cache with two call sites.
    preview: Option<Preview>,
    /// The text layer this panel is currently *editing*, if it is editing one.
    ///
    /// See [`Editing`]. `None` is the composing state, which is what the panel
    /// has always been.
    pub editing: Option<Editing>,
    /// The composing state, put aside while a text layer is selected.
    ///
    /// **Put aside rather than overwritten**, because a block being composed
    /// belongs to the person and a layer's record belongs to the picture — the
    /// same division that keeps [`TextState`] above the `--- documents ---`
    /// line. Clicking a text layer to fix a typo and losing the caption you
    /// were half way through typing is the failure this exists to prevent.
    stashed: Option<Composed>,
}

/// The three fields that say what a block of text *is*, as the panel holds them.
///
/// A struct only so the composing state can be swapped out whole while a layer's
/// record is being edited: the panel body reads [`TextState::block`],
/// [`TextState::family`] and [`TextState::style`] and does not need to know
/// which of the two it is drawing, which is what keeps this change out of every
/// control in the module.
#[derive(Clone, Debug)]
pub struct Composed {
    pub block: TextBlock,
    pub family: String,
    pub style: String,
}

/// A text layer the panel is editing, and what it was when it was picked up.
///
/// **Keyed by the document and the *slot*, never by the row's position.** Stack
/// order is a `Vec` order, so a reorder would otherwise make the panel show one
/// layer's record while Update wrote to another; a slot never changes hands
/// while the layer holds it. The document is in the key because a slot is a
/// slice of one document's texture array, so slot 3 is a different layer in
/// every tab — exactly the reason `Thumbs`' cache is keyed by document.
pub struct Editing {
    pub doc: crate::session::DocId,
    pub slot: u32,
    /// The record as it was when this layer was picked up. What Update replaces,
    /// what an undo puts back, and what "has anything actually changed" is
    /// measured against.
    pub original: umber_core::textobj::TextObject,
    /// The colour the edited text will be set in. Starts as the record's own —
    /// see [`Editor::text_colour`](crate::editor::Editor::text_colour).
    pub colour: umber_core::Color,
}

/// What the panel drew, and what it had to rasterise twice to be able to say.
///
/// **The measured size and the notices are cached beside the picture, and that
/// is not tidiness.** Both come from setting the block at its *real* size —
/// which for a 72 px caption on a large canvas is a rasterisation measured in
/// megapixels — and a panel body runs on every frame the module is open. Read
/// afresh each time, opening the Text panel would rasterise somebody's caption
/// sixty times a second for as long as they left it open, which is precisely
/// the class of cost the "nothing on the drawing path allocates per frame" rule
/// is about.
struct Preview {
    key: u64,
    /// `None` where the block would not set at all — nothing typed, no ink, or
    /// past the cap. The panel then draws no picture and no figure.
    picture: Option<egui::TextureHandle>,
    /// What it will measure on the canvas, or why it would not set at all —
    /// from the same call that made the picture's smaller twin, so the figure
    /// and the notices cannot come from a different block than the one on
    /// screen.
    ///
    /// **The refusal is kept rather than discarded**, and that is the whole of
    /// what the panel used to get wrong. `build_preview` already sets the real
    /// block at its real size, so by the time anything is drawn the answer is
    /// known — and it was thrown away. A block past `text::MAX_PIXELS` then
    /// drew a *picture* anyway, because the preview rasterises at
    /// [`PREVIEW_EM`] and 26 pixels succeeds where 1000 does not, silently
    /// dropped the "N × M px on the canvas" line, left Place live, and told
    /// nobody until they clicked it. A font moved off the disk since the scan
    /// said nothing at all.
    ///
    /// One `Result` rather than two `Option`s documented as complementary:
    /// they come from the two arms of one, and this way there is no fourth
    /// state for a reader to have to draw nothing for.
    measured: Result<(u32, u32), Refused>,
    missing: Vec<char>,
    mixed: bool,
}

/// A refusal and the sentence for it, worked out once.
///
/// The sentence is cached beside the error for the reason everything else in
/// [`Preview`] is. It is drawn under the preview *and* handed to the disabled
/// button as its tooltip, on every frame the panel is open, and [`refusal`]
/// builds a `String`: twice a frame for as long as somebody leaves the panel up
/// is the per-frame allocation the rest of this module is careful about.
struct Refused {
    err: TextError,
    line: String,
}

impl Refused {
    fn new(err: TextError) -> Self {
        Self {
            err,
            line: refusal(err),
        }
    }
}

impl Default for TextState {
    fn default() -> Self {
        Self {
            block: TextBlock::default(),
            family: "Archivo".to_string(),
            style: "Regular".to_string(),
            fonts: Fonts::default(),
            search: String::new(),
            loaded: None,
            preview: None,
            editing: None,
            stashed: None,
        }
    }
}

impl TextState {
    /// The face the current family and style name, resolved against whatever
    /// the library holds now.
    pub fn face(&self) -> Option<&Face> {
        self.fonts.library().resolve(&self.family, &self.style)
    }

    /// The face in hand and its bytes, read from disk at most once per
    /// (family, style, library).
    ///
    /// `&mut self` because it fills the cache; see [`TextState::loaded`] for
    /// why there is one.
    fn face_and_data(&mut self) -> Option<(Face, &umber_core::fonts::FontData)> {
        let face = self.face()?.clone();
        let generation = self.fonts.generation();
        let stale = match &self.loaded {
            Some((family, style, held, _)) => {
                *held != generation || *family != face.family || *style != face.style
            }
            None => true,
        };
        if stale {
            let data = face.load()?;
            self.loaded = Some((face.family.clone(), face.style.clone(), generation, data));
        }
        self.loaded.as_ref().map(|(.., data)| (face, data))
    }

    /// Rasterise the block at its real size, ready to be placed.
    ///
    /// Blocking on the rasteriser, and on a file read the first time a face is
    /// used, which is what an explicit click may do.
    pub fn set(&mut self) -> Result<umber_core::text::Setting, TextError> {
        self.set_and_record().map(|(setting, _)| setting)
    }

    /// The same, with the record of **which face it was actually set in**.
    ///
    /// The two together rather than a second lookup beside [`Self::set`],
    /// because they have to agree: `FontLibrary::resolve` is total, so the face
    /// the pixels were made with is not necessarily the pair the pickers name,
    /// and a record naming the picker's choice would re-render somebody's
    /// caption in a different typeface the day that font turned up.
    ///
    /// The PostScript name is read here because this is where the font's bytes
    /// are already in hand — see [`umber_core::textobj::postscript_name`], which
    /// exists for exactly that reason.
    pub fn set_and_record(
        &mut self,
    ) -> Result<(umber_core::text::Setting, umber_core::textobj::TextFace), TextError> {
        // Cloned before the borrow, because `face_and_data` fills the cache and
        // therefore holds `self` — a paragraph is cloned once per Place, which
        // is a click, not once per frame.
        let block = self.block.clone();
        let (face, data) = self.face_and_data().ok_or(TextError::Unreadable)?;
        let record = umber_core::textobj::TextFace::of(
            &face,
            data.font()
                .as_ref()
                .map(umber_core::textobj::postscript_name)
                .unwrap_or_default(),
        );
        text::set(&face, data, &block).map(|setting| (setting, record))
    }
}

/// Point the panel at whatever the Layers panel has selected.
///
/// A text layer selected means the panel shows **that layer's** record and its
/// controls act on it; anything else means the composing state, which is what
/// the panel has always been. Called once at the top of the body, so there is
/// one place the two can be swapped and no control below has to know which it
/// is drawing.
///
/// **The switch is a swap, not a copy.** What is composed is stashed and comes
/// back; what is edited is discarded, because the layer still holds the record
/// it was read from and re-reading it is the honest answer to "what does this
/// layer say". Selecting a second text layer therefore abandons unapplied edits
/// to the first — which is the only behaviour that cannot show one layer's
/// record while writing to another, and it is why the key below carries the
/// slot rather than the row.
fn sync_editing(ed: &mut Editor) {
    let doc = ed.session.active_id();
    // The selected entry, and only if it is a text layer whose slot is real.
    // A folder holds neither, and `set_text` already refuses one.
    let at = ed.layers.active_index();
    let target = ed
        .layers
        .active_slot()
        .filter(|_| ed.layers.active_text().is_some())
        .map(|slot| (doc, slot));
    let held = ed.text.editing.as_ref().map(|e| (e.doc, e.slot));
    // **The record can change under the panel, and an undo is how.** Stepping
    // back over a text edit puts the older record on the layer; the panel would
    // otherwise go on showing the newer caption over a canvas that no longer has
    // it, with Update reading "nothing has changed" and therefore disabled — the
    // two irreconcilable, and the panel the one telling the lie. A canvas flip
    // is the same shape one field along, since it mirrors the placement.
    //
    // Compared against `original` and never against what the controls hold: the
    // artist is typing into those, so a comparison there would reload the panel
    // out from under every keystroke.
    let adrift = ed
        .text
        .editing
        .as_ref()
        .is_some_and(|e| ed.layers.active_text() != Some(&e.original));
    if held == target && !adrift {
        return;
    }
    // Off whatever was being edited: the composing block comes back.
    if ed.text.editing.take().is_some()
        && let Some(back) = ed.text.stashed.take()
    {
        ed.text.block = back.block;
        ed.text.family = back.family;
        ed.text.style = back.style;
    }
    let Some((doc, slot)) = target else {
        return;
    };
    let Some(record) = ed.layers.text_at(at).cloned() else {
        return;
    };
    ed.text.stashed = Some(Composed {
        block: std::mem::take(&mut ed.text.block),
        family: std::mem::take(&mut ed.text.family),
        style: std::mem::take(&mut ed.text.style),
    });
    ed.text.block = record.block.clone();
    ed.text.family = record.face.family.clone();
    ed.text.style = record.face.style.clone();
    ed.text.editing = Some(Editing {
        doc,
        slot,
        colour: record.colour,
        original: record,
    });
}

/// The Text panel body.
pub fn panel(ui: &mut Ui, p: &Palette, ed: &mut Editor, actions: &mut UiActions) {
    // Before anything is drawn, so every control below acts on the block the
    // panel is actually showing.
    sync_editing(ed);
    // The scan starts the first time somebody opens this module, not at
    // start-up: it is several hundred file reads for a feature most sessions
    // never reach.
    ed.text.fonts.start(&ed.font_folder);
    if ed.text.fonts.poll() {
        ui.ctx().request_repaint();
    }
    if ed.text.fonts.scanning() {
        // Bounded, and only while this panel is on screen — the cost
        // `render`'s `repaint_at` exists to avoid is a *perpetual* request, and
        // this one ends when the worker does.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(150));
    }

    // A real `TextEdit`, for everything in the module docs: the caret, the
    // selection, the IME and the system clipboard, none of which is worth
    // reimplementing. `ui::draw` already calls `shortcuts::set_typing` from
    // `text_edit_focused`, so typing "brush" in here does not also pick the
    // brush.
    let edit = egui::TextEdit::multiline(&mut ed.text.block.text)
        .desired_rows(3)
        .desired_width(ui.available_width())
        .hint_text("Type something");
    ui.add(edit);
    ui.add_space(8.0);

    font_picker(ui, p, ed);
    ui.add_space(2.0);
    style_picker(ui, p, ed);
    // **Composing only.** That note says "Umber is setting this text in X
    // instead", which is true of Place and false of a text layer: `edit_row`
    // refuses Update outright when the pickers name a font this machine has not
    // got, so nothing is set in a substitute and the sentence would be a
    // promise about a click that is not offered. The two sentences a text layer
    // needs are `edit_row`'s own.
    if ed.text.editing.is_none() {
        substitution_note(ui, p, ed);
    }
    ui.add_space(6.0);

    // The rails write into the block's own fields. Taking a copy and putting it
    // back is the obvious shape and clones the artist's whole paragraph on
    // every frame the panel is open, which is what the drawing path may not do.
    widgets::number_row(
        ui,
        p,
        &mut ed.text.block.size,
        NumberRow {
            label: "Size",
            range: text::MIN_SIZE..=text::MAX_SIZE,
            snap: 1.0,
            per_unit: 1.0,
            suffix: " px",
            decimals: 0,
            deferred: false,
        },
    );
    widgets::number_row(
        ui,
        p,
        &mut ed.text.block.line_spacing,
        NumberRow {
            label: "Line spacing",
            range: 0.5..=3.0,
            snap: 0.05,
            per_unit: 100.0,
            suffix: "%",
            decimals: 0,
            deferred: false,
        },
    );
    widgets::number_row(
        ui,
        p,
        &mut ed.text.block.tracking,
        NumberRow {
            label: "Tracking",
            range: -20.0..=40.0,
            snap: 0.5,
            per_unit: 1.0,
            suffix: " px",
            decimals: 1,
            deferred: false,
        },
    );
    ui.add_space(4.0);
    widgets::segmented(
        ui,
        p,
        &mut ed.text.block.align,
        &[
            (Align::Left, Align::Left.label()),
            (Align::Centre, Align::Centre.label()),
            (Align::Right, Align::Right.label()),
        ],
    );

    ui.add_space(10.0);
    // The refusal is *handed* to the row rather than read back out of the
    // cache. `preview` is the only thing that knows whether the cache it just
    // drew from describes what is typed now — it returns early on an empty
    // block without rebuilding — so passing the answer along is one statement
    // where reading `Editor::text.preview` again would be a second, correct
    // only for as long as these two calls stay in this order.
    let refused = preview(ui, p, ed);

    ui.add_space(8.0);
    if ed.text.editing.is_some() {
        edit_row(ui, p, ed, refused, actions);
    } else {
        place_row(ui, p, ed, refused, actions);
    }
}

/// The two controls a **text layer** gets in place of Place.
///
/// Place is not drawn at all here rather than drawn disabled, which is the
/// opposite call the style marks get and the same one a folder's missing
/// opacity gets: there is something else in that place doing the job, so an
/// extra dead button would be a control with nothing to say. `begin_float`
/// still refuses a paste onto this layer, and says why — the gate catches the
/// route that goes round the panel.
fn edit_row(
    ui: &mut Ui,
    p: &Palette,
    ed: &Editor,
    refused: Option<TextError>,
    actions: &mut UiActions,
) {
    let editing = ed.text.editing.as_ref().expect("the caller checked");
    let frozen = editing
        .original
        .face
        .resolve(ed.text.fonts.library())
        .is_none();
    // **The pair the *pickers* name, which is what `update_text_layer` actually
    // resolves.** The two readings are usually the same face, because the
    // pickers were loaded from the record; but the artist can scroll to a family
    // this machine has not got, and a control read off the record alone would
    // then be live and the click would raise a dialog. That is the `plan_`/`can_`
    // arrangement one step along: the control asks whether it may, and the act
    // asks what to do, off the same reading.
    let face_here = umber_core::textobj::TextFace {
        family: ed.text.family.clone(),
        style: ed.text.style.clone(),
        postscript: String::new(),
    }
    .resolve(ed.text.fonts.library())
    .is_some();
    if frozen {
        // **Named before anything is pressed, not after.** The face the record
        // asks for is not on this machine, so the saved pixels are all there is
        // until somebody either installs it or picks another one deliberately.
        controls::note(ui, p, &editing.original.face.missing_notice());
        ui.add_space(6.0);
    }
    // A record carries its own colour, so setting it again in whatever happens
    // to be in the palette would repaint the caption every time somebody fixed
    // a typo. This is how the artist asks for that instead.
    let same = ed.color.to_srgb_u8() == editing.colour.to_srgb_u8();
    let locked = ed.layers.active_is_locked();
    let state = update_state(
        locked,
        face_here,
        ed.text.block.text.trim().is_empty(),
        refused,
        unchanged(ed),
    );
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let response = controls::text_button(ui, p, "Update text", true, state.enabled);
            if response.clicked() {
                actions.update_text = true;
            }
            response.on_hover_text(state.tooltip.as_ref());

            let colour = controls::text_button(ui, p, "Colour in hand", false, !same);
            if colour.clicked() {
                actions.take_text_colour = true;
            }
            colour.on_hover_text(if same {
                "The text is already set in the colour the palette is holding."
            } else {
                "Set this text in the colour the palette is holding, the next time \
                 Update is pressed."
            });
        });
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Gated on the lock like everything else that edits a layer. It
            // changes no pixel, and it changes what the file carries and what
            // may be painted here afterwards, which is what a lock is about.
            let response = controls::text_button(ui, p, "Convert to paint", false, !locked);
            if response.clicked() {
                actions.convert_text_to_paint = true;
            }
            response.on_hover_text(if locked {
                "This layer is locked, or it is inside a locked folder. Unlock it in \
                 the Layers panel."
            } else {
                "Keep every pixel and stop treating this layer as text. It can then be \
                 painted and pasted on, and it cannot be set again."
            });
        });
    });
}

/// Whether anything in the panel differs from the record the layer holds.
///
/// What keeps Update from being a live button that would record an undo entry
/// for replacing a caption with itself. Read off the *record* rather than a
/// dirty flag, so there is nothing to fall out of step with what the controls
/// wrote — and it compares the fields a re-render actually reads, which is why
/// the placement is not among them.
fn unchanged(ed: &Editor) -> bool {
    let Some(editing) = ed.text.editing.as_ref() else {
        return false;
    };
    let was = &editing.original;
    ed.text.block == was.block
        && ed.text.family.eq_ignore_ascii_case(&was.face.family)
        && ed.text.style.eq_ignore_ascii_case(&was.face.style)
        && editing.colour.to_srgb_u8() == was.colour.to_srgb_u8()
}

/// Whether Update may be pressed, and what to say about it.
///
/// A pure function of four readings, separate from [`edit_row`] for the reason
/// [`place_state`] is separate from [`place_row`]: the decision is the part that
/// can be wrong, and reading it out of a running window is not a test anybody
/// can run on CI.
///
/// The order is the order of the sentences somebody would want. A missing font
/// comes first because nothing else about the layer can be acted on until it is
/// there; "nothing has changed" comes last, because it is the only one that is
/// not a problem.
///
/// **The font reading is the pair the *pickers* name, and that is the one
/// `update_text_layer` resolves.** A layer whose recorded face is gone opens
/// with that face in the boxes, so this refuses it and the notice above the row
/// names the font — the layer is frozen, and the saved pixels stand.
///
/// What it deliberately does *not* do is stay frozen once the artist has chosen
/// a font that **is** here. The rule text lives by is that a caption is never
/// re-rendered in a face its author did not choose *silently*; picking one off
/// the list is not silent, and it is the only way back for somebody who has the
/// document and not the font. Reading the record's face instead left the family
/// and style dropdowns live and doing nothing, which is the control that lies.
fn update_state(
    locked: bool,
    face_here: bool,
    empty: bool,
    refused: Option<TextError>,
    unchanged: bool,
) -> Place {
    if locked {
        return Place {
            enabled: false,
            tooltip: "This layer is locked, or it is inside a locked folder. Unlock it \
                      in the Layers panel, or select another layer."
                .into(),
        };
    }
    if !face_here {
        return Place {
            enabled: false,
            tooltip: "The font named in the boxes above is not on this machine. Umber \
                      will not set this text in a substitute, because a caption redrawn \
                      in another face is a change to the picture. Choose a font the \
                      list has."
                .into(),
        };
    }
    if empty {
        return Place {
            enabled: false,
            tooltip: "Type something first. To take the text off the canvas, undo the \
                      edit that put it there."
                .into(),
        };
    }
    if let Some(err) = refused {
        return Place {
            enabled: false,
            tooltip: refusal(err).into(),
        };
    }
    if unchanged {
        return Place {
            enabled: false,
            tooltip: "Nothing has changed, so there is nothing to set again.".into(),
        };
    }
    Place {
        enabled: true,
        // It says what moving it costs, because the tempting reading of "where
        // it already is" is that the transform tool is beside it doing the
        // other half — and picking the caption up with that tool turns the
        // layer into paint, since a placement cannot hold the shear a second
        // rotation and scale would put into it.
        tooltip: "Draw this text again on the layer, where it already is. Moving it \
                  with the transform tool makes the layer paint."
            .into(),
    }
}

/// Which family: a search field, and under it the dropdown this interface has
/// one of.
///
/// **The field is above the trigger and not inside the menu**, and that is the
/// difference between a control and a control-shaped thing.
/// `widgets::dropdown` opens with `egui::Popup::menu`, whose default close
/// behaviour is `CloseOnClick` — *any* click, one inside the popup included.
/// That is exactly right for the `selectable_label`s every other call site puts
/// in a menu, and it means a text field in there can never be typed into at
/// all: the click that would focus it is the click that shuts the menu.
/// Outside, it is an ordinary widget, it stays put while the list is scrolled,
/// and it is still there to be adjusted after a look at the list.
///
/// The menu therefore holds only rows, and needs no scroll area of its own —
/// `widgets::dropdown` already wraps the body in one at `metrics::DROPDOWN_MENU`.
/// A second, *taller* one inside it was two bars over one list and a wheel that
/// meant two things.
fn font_picker(ui: &mut Ui, p: &Palette, ed: &mut Editor) {
    controls::search_field(ui, p, &mut ed.text.search, "Search fonts");
    ui.add_space(4.0);

    // Borrowed field by field, so the trigger can hold `&family` while the menu
    // body reads the library — and so neither is cloned on a frame that changes
    // nothing.
    let crate::textpanel::TextState {
        family,
        fonts,
        search,
        ..
    } = &ed.text;
    // **Nothing here allocates while the search field is empty**, which is the
    // state a panel left open sits in, and a real machine is what makes that
    // matter: several hundred families, and a body that runs every frame for as
    // long as somebody is composing. This used to build a `Vec` of every family
    // name, a lowered `String` per family, a lowered `String` for the query,
    // and a `String` of a figure `Fonts` caches as a `String` precisely so it
    // need not be formatted — per frame, all of it, and the count was computed
    // and thrown away whenever the field was empty.
    //
    // With a query typed there is one `String` a frame, for the figure, and
    // that is left rather than cached: it changes with every keystroke, so a
    // cache would be a second copy of the filter to keep in step for one small
    // allocation while somebody is actively typing.
    let query = search.trim();
    // Only counted when there is something to count. With the field empty the
    // answer is the cached figure, and walking the library to arrive at the
    // same number is the work this line exists not to do.
    let matching = (!query.is_empty()).then(|| {
        fonts
            .library()
            .families_iter()
            .filter(|name| widgets::contains_ignore_case(name, query))
            .count()
            .to_string()
    });
    // The figure is what the filter is leaving rather than what the machine
    // holds, because the filter is now a control somebody can see above it —
    // a count that did not move as they typed would say the field did nothing.
    let count: &str = matching.as_deref().unwrap_or_else(|| fonts.family_count());
    let mut chosen: Option<String> = None;
    widgets::dropdown(
        ui,
        p,
        widgets::Dropdown::new(family)
            .icon(Icon::Text)
            .trailing(count)
            .width(DropdownWidth::Fill),
        |ui| {
            for name in fonts.library().families_iter() {
                if !widgets::contains_ignore_case(name, query) {
                    continue;
                }
                if ui
                    .selectable_label(name.eq_ignore_ascii_case(family), name)
                    .clicked()
                {
                    chosen = Some(name.to_string());
                }
            }
        },
    );
    if let Some(family) = chosen {
        ed.text.family = family;
        // The style is a name within a family, so it cannot survive the family
        // changing. Landing on whatever `resolve` picks — nearest to regular,
        // upright — keeps the panel showing the face it is actually going to
        // set rather than one it will substitute for silently.
        ed.text.style = ed
            .text
            .fonts
            .library()
            .resolve(&ed.text.family, "Regular")
            .map(|f| f.style.clone())
            .unwrap_or_else(|| "Regular".to_string());
    }
}

/// Which style within the family, and the two marks that reach for the obvious
/// ones directly.
///
/// The list is walked **inside** the menu body, which only runs while the menu
/// is open. Collecting it outside is the shape that reads more naturally and
/// builds a `String` per style of the family on every frame the panel is
/// open — a variable font is nine of them, and the panel is open for as long as
/// somebody is composing.
///
/// **The dropdown is the whole control and the toggles are a shortcut into it.**
/// A family's bold may be called Bold, Demi, Heavy, Black, Gras or Negrita, so
/// the list has to be there; but "make this bold" is what somebody actually
/// wants, and hunting for the right word in a list of nine is not that. The two
/// marks write the same field the list does — [`TextState::style`] stays the one
/// source of truth — and there is no third piece of state saying whether the
/// text "is bold".
fn style_picker(ui: &mut Ui, p: &Palette, ed: &mut Editor) {
    let mut chosen = None;
    let mut pressed = None;
    {
        let crate::textpanel::TextState {
            family,
            style,
            fonts,
            ..
        } = &ed.text;
        let library = fonts.library();
        // Over the enum rather than a pair written out here, so the marks and the
        // states `emphasis_tip` has to have a sentence for are the same set.
        let marks = Emphasis::ALL.map(|which| (which, emphasis(library, family, style, which)));
        ui.horizontal(|ui| {
            // Sized by the controls beside it rather than by the layout, which
            // is `DropdownWidth::Exact`'s own case: it shares this line with the
            // marks and `Fill` would take the room they want. Counted off
            // `Emphasis::ALL` and read afresh here rather than from a constant,
            // for the reason the selection strip's combine line reads
            // `available_width` after its hint.
            let taken = marks.len() as f32 * (widgets::ICON_TOGGLE + ui.spacing().item_spacing.x);
            let room = (ui.available_width() - taken).max(0.0);
            widgets::dropdown(
                ui,
                p,
                widgets::Dropdown::new(style).width(DropdownWidth::Exact(room)),
                |ui| {
                    for face in library.family(family) {
                        if ui
                            .selectable_label(face.style.eq_ignore_ascii_case(style), face.label())
                            .clicked()
                        {
                            chosen = Some(face.style.clone());
                        }
                    }
                },
            );
            for (which, toggle) in &marks {
                let icon = which.icon();
                if widgets::icon_toggle(ui, p, icon, toggle.on, toggle.enabled(), toggle.tip) {
                    pressed = Some(*which);
                }
            }
        });
    }
    // The list wins if somebody managed both in one frame: it is the explicit
    // choice, where a mark is a shorthand for one.
    if let Some(style) = chosen {
        ed.text.style = style;
        return;
    }
    if let Some(which) = pressed {
        apply_emphasis(ed, which);
    }
}

/// Carry out what pressing one of the two style marks means.
///
/// Separate from [`style_picker`] so a test can press it: the only thing about
/// this that can be wrong is which face it lands on, and that is not a thing to
/// read out of a running window.
///
/// It asks [`emphasis`] again rather than being handed the answer the control
/// was drawn from — the same function, so the same decision, which is the
/// `plan_`/`can_` arrangement one step along: the control asks whether it may,
/// and the act asks what to do. False where the mark was refused; a disabled
/// control cannot report that, but a keystroke route to the same command one day
/// could.
fn apply_emphasis(ed: &mut Editor, which: Emphasis) -> bool {
    let target = {
        let library = ed.text.fonts.library();
        let mark = emphasis(library, &ed.text.family, &ed.text.style, which);
        // **The refusal is honoured here, not only drawn.** `restyle` is not the
        // whole gate: with no face in hand it still finds the family's bold
        // perfectly well, so reading only its answer left a *refused* mark able
        // to rewrite `TextState::style` — which throws away the choice
        // `substitution_note` promises is kept. Asking the same `Toggle` the
        // control was drawn from is what makes "disabled" mean it.
        mark.enabled()
            .then(|| {
                library
                    .restyle(&ed.text.family, &ed.text.style, mark.want.0, mark.want.1)
                    .map(|face| face.style.clone())
            })
            .flatten()
    };
    match target {
        Some(style) => {
            ed.text.style = style;
            true
        }
        None => false,
    }
}

/// Which of the two style marks is being asked about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Emphasis {
    Bold,
    Italic,
}

impl Emphasis {
    /// Both of them, so `style_picker` draws one mark per variant and a test
    /// enumerating the states a mark can be in reaches every variant.
    ///
    /// **A hand-written array, guarded by an exhaustive match in
    /// `every_state_a_style_mark_can_be_in_has_a_finished_sentence`** — the rule
    /// CLAUDE.md states, because a test that merely *iterates* `ALL` can only
    /// check what is in it, and a third variant left out of this would simply
    /// not be drawn. The arms there index it, so a short array is an
    /// out-of-bounds panic rather than a quiet pass.
    const ALL: [Emphasis; 2] = [Emphasis::Bold, Emphasis::Italic];

    fn icon(self) -> Icon {
        match self {
            Emphasis::Bold => Icon::Bold,
            Emphasis::Italic => Icon::Italic,
        }
    }
}

/// What one of the two style marks is showing, and what to say about it.
struct Toggle {
    /// Whether the face in hand already is this. Read off the library rather
    /// than held, so there is nothing to fall out of step with the style name.
    on: bool,
    /// `None` where the mark may be pressed. See [`Lacking`].
    lacking: Option<Lacking>,
    /// `&'static str` and not a `format!`, because this row is drawn on every
    /// frame the panel is open and two `String`s a frame is the per-frame
    /// allocation the rest of this module is careful about. Naming the target
    /// style would read a little better and the dropdown beside it says the
    /// name a moment later anyway.
    tip: &'static str,
    /// The `(bold, italic)` pair pressing it asks
    /// [`FontLibrary::restyle`](umber_core::fonts::FontLibrary::restyle) for.
    ///
    /// Carried on the answer rather than worked out again at the press, so the
    /// control and the act cannot disagree about what was offered.
    want: (bool, bool),
}

impl Toggle {
    fn enabled(&self) -> bool {
        self.lacking.is_none()
    }
}

/// What is missing where a style mark may not be pressed.
///
/// **The refusal is the sentence, so the sentence has to be true**, and one
/// "enabled" boolean could not carry a true one. What `can_restyle` answers is
/// "is there such a face *at this slant and on that side of the weight*", where
/// the sentences were written as though it answered about the whole family — so
/// Bold on an italic face, in a family carrying a bold and no bold italic, read
/// "this family has no bold on this machine. Install the family's bold weight",
/// with that bold two rows above in the list beside it. The missing-family case
/// said the same thing about a font that was not installed at all.
///
/// Four sentences each, because they send somebody four different places: fetch
/// a font, choose another one, pick a different row of this list, or nothing at
/// all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lacking {
    /// The family the picker names is not on this machine, so there is no face
    /// of it to be bold or italic. The substitution note above the marks is what
    /// says the family is missing; this is only the tooltip.
    Family,
    /// The family **is** here and the style recorded beside it is not, so there
    /// is no face in hand to read a weight or a slant off.
    ///
    /// Its own state and not [`Self::Family`]'s, which is where it used to land:
    /// `FontLibrary::exact` answers about the *pair*, so reading its `None` as a
    /// missing family put "that font is not on this machine, choose a font the
    /// list has" beside a picker naming a font that was, and inches under
    /// `substitution_note` saying the opposite in the same panel. It is reachable
    /// exactly where that note is — the library is replaced wholesale when a scan
    /// lands and when the folder preference changes, which is what
    /// `Fonts::generation` exists for.
    ///
    /// **Both marks are refused here**, and Bold used to be live: `restyle` finds
    /// the family's bold perfectly well with no face in hand, so a press
    /// rewrote `TextState::style` to a real name. That silently discards the
    /// choice `substitution_note` promises is kept.
    Style,
    /// The family carries no face of this emphasis, at any weight or slant.
    Any,
    /// It carries one, but not to go with the face in hand: a bold and no bold
    /// italic, or an italic at other weights and not at this one.
    Pairing,
    /// The face in hand *is* this, and the family has nothing to match it
    /// without it — no lighter face at this slant, or no upright one at this
    /// weight. A family of one heavy display weight, or an italic-only script
    /// face, are the whole-family versions; a family whose every upright is
    /// SemiBold or heavier is the ordinary one.
    WayBack,
}

/// Whether Bold or Italic may be pressed for this `(family, style)`, and what to
/// say about it.
///
/// A pure function of the library and two names, separate from what draws it for
/// the reason [`place_state`] is: the decision is the part that can be wrong,
/// and reading it out of a running window is not a test anybody can run on CI.
///
/// **A refused mark is drawn disabled rather than not drawn at all**, which is
/// the opposite of the call a folder's missing opacity control gets, and the
/// difference is what the absence would mean. There are two of these and they
/// are always the same two, so one of them missing reads as "you have not found
/// it yet"; and the disabled state carries real news — *this typeface has no
/// italic on this machine* — which is a thing somebody needs to know before
/// they go looking for the setting that would produce one. There is none, in
/// Umber or anywhere honest: see `FontLibrary::restyle` for why a sheared
/// upright and a smeared outline are refused.
fn emphasis(library: &FontLibrary, family: &str, style: &str, which: Emphasis) -> Toggle {
    // The *exact* face, never `resolve`'s substitute: these marks are about the
    // family the picker names, and the substitution note above them is what
    // says that family is not here. Reaching into the substitute would make
    // Bold light up for a typeface the artist has not chosen.
    let face = library.exact(family, style);
    let italic = face.is_some_and(|f| f.italic);
    let (on, want) = match which {
        // **The anchor, not `Face::is_bold`.** A nine-weight family has four
        // faces on the bold side of the threshold, and lighting from that said
        // SemiBold was bold and then took a press as "make it regular" — so no
        // press reached Bold from SemiBold at all. See
        // `FontLibrary::is_bold_anchor`.
        Emphasis::Bold => {
            let on = library.is_bold_anchor(family, style);
            (on, (!on, italic))
        }
        // The slant genuinely is a property of the face, so this one is read off
        // it. Which candidate pool the press draws from is the face's own half of
        // the family, which is what keeps Light Italic for Light.
        Emphasis::Italic => {
            let bold = face.is_some_and(|f| f.is_bold());
            (italic, (bold, !italic))
        }
    };
    // **The two no-face states come first, before `can_restyle` is even asked.**
    // With nothing in hand `restyle` still finds the family's bold, so asking it
    // first left Bold live over a stale style name and a press rewrote the
    // artist's kept choice. There is also no face to read `on` off, so a lit or
    // unlit mark there would be a reading of nothing.
    let lacking = if face.is_none() {
        Some(if library.has_family(family) {
            Lacking::Style
        } else {
            Lacking::Family
        })
    } else if library.can_restyle(family, style, want.0, want.1) {
        None
    } else if on {
        // The press asks to take this off, so what the family does or does not
        // carry *of* this emphasis is the wrong question.
        Some(Lacking::WayBack)
    } else if match which {
        Emphasis::Bold => library.offers_bold(family),
        Emphasis::Italic => library.offers_italic(family),
    } {
        Some(Lacking::Pairing)
    } else {
        Some(Lacking::Any)
    };
    Toggle {
        on,
        lacking,
        tip: emphasis_tip(which, lacking),
        want,
    }
}

/// The sentence for each of the twelve states one of these marks can be in.
///
/// Written out rather than assembled from clauses, and matched with **no
/// wildcard**, so a state cannot be added without a sentence being written for
/// it. Each refusal says what is actually missing and, where there is one to
/// name, what Umber will not do instead: "this control is dead" with no reason
/// is the thing a disabled control is only worth drawing to avoid.
///
/// **Every sentence is scoped to what was actually checked**, which is the fix
/// the `Lacking` enum exists for and which its first draft still got wrong twice.
/// `can_restyle` answers about this slant on that side of [`BOLD_THRESHOLD`],
/// never about the whole family, so [`Lacking::WayBack`] says "to match it"
/// rather than claiming a total absence: a family of SemiBold and Bold refuses
/// the lit Bold and *does* have something lighter, and one with an upright Bold
/// beside an italic Regular refuses the lit Italic and *does* have an upright.
///
/// The two `WayBack` arms name nothing Umber refuses, and that is right rather
/// than an omission: taking an emphasis *off* needs no synthesis, so there is no
/// fake to decline. What is missing there is a face to match the one in hand.
fn emphasis_tip(which: Emphasis, lacking: Option<Lacking>) -> &'static str {
    match (which, lacking) {
        (Emphasis::Bold, None) => "Set this text in the family's own bold, or take it off again.",
        (Emphasis::Bold, Some(Lacking::Family)) => {
            "That font is not on this machine, so Umber has nothing of it to set in bold. \
             Choose a font the list has."
        }
        (Emphasis::Bold, Some(Lacking::Style)) => {
            "This family has no style by the name in the box above, so there is nothing in \
             hand to make bold. Choose a style from the list."
        }
        (Emphasis::Bold, Some(Lacking::Any)) => {
            "This family has no bold on this machine. Umber will not thicken an outline to \
             fake one, because a smeared letter looks wrong at every size. Install the \
             family's bold weight, or choose another font."
        }
        (Emphasis::Bold, Some(Lacking::Pairing)) => {
            "This family has a bold, but not to go with the style in hand. Umber will not \
             thicken an outline to fake one. Pick the weight you want from the list."
        }
        (Emphasis::Bold, Some(Lacking::WayBack)) => {
            "This family has no lighter weight to match the style in hand on this machine, \
             so there is nothing to take the bold off to. Pick a style from the list."
        }
        (Emphasis::Italic, None) => {
            "Set this text in the family's own italic, or take it off again."
        }
        (Emphasis::Italic, Some(Lacking::Family)) => {
            "That font is not on this machine, so Umber has nothing of it to set in italic. \
             Choose a font the list has."
        }
        (Emphasis::Italic, Some(Lacking::Style)) => {
            "This family has no style by the name in the box above, so there is nothing in \
             hand to make italic. Choose a style from the list."
        }
        (Emphasis::Italic, Some(Lacking::Any)) => {
            "This family has no italic on this machine. Umber will not slant an upright face \
             to fake one, because a sheared letter is not the shape its designer drew. \
             Install the family's italic, or choose another font."
        }
        (Emphasis::Italic, Some(Lacking::Pairing)) => {
            "This family has an italic, but not at the weight in hand. Umber will not slant \
             an upright face to fake one. Pick a weight it has, or choose the italic from \
             the list."
        }
        (Emphasis::Italic, Some(Lacking::WayBack)) => {
            "This family has no upright face to match the style in hand on this machine, so \
             there is nothing to take the italic off to. Pick a style from the list."
        }
    }
}

/// Say so when the face being set in is not the one the pickers name.
///
/// `FontLibrary::resolve` is total on purpose — a preference records names and
/// the machine it is read back on may have neither — so it always answers, and
/// its own documentation says the **caller** is what says a substitution
/// happened. This is that caller, and until now it did not say it.
///
/// It is reachable rather than theoretical. The library is replaced wholesale
/// when the scan lands and again when the font-folder preference changes, so a
/// family chosen from the old list can simply stop existing; the dropdown goes
/// on showing the name that was picked while the preview, the measurement and
/// Place all use something else. Naming both is the import rule applied here.
///
/// **The choice is not rewritten to the resolved name.** Somebody who picked a
/// face and then pointed Umber at a different folder should get their face back
/// when they point it home again, and a picker that quietly rewrote itself
/// would have thrown that away with no way to notice.
fn substitution_note(ui: &mut Ui, p: &Palette, ed: &Editor) {
    let library = ed.text.fonts.library();
    // The face comes back with the answer, so this names the one the reading
    // was actually taken from rather than resolving a second time and hoping
    // the two agree.
    let Some((what, face)) = library.substituted(&ed.text.family, &ed.text.style) else {
        return;
    };
    // Two sentences and not one with a hole in it, because the two are
    // different pieces of news. A family that is not here is a font to go and
    // find; a family that is here without the style asked for is a weight the
    // typeface never had, and telling somebody to install it would send them
    // looking for something that does not exist.
    let body = match what {
        Substitution::Family => format!(
            "{} is not on this machine. Umber is setting this text in {} {} instead. \
             Your choice is kept, so the text goes back to it if the font turns up again.",
            ed.text.family, face.family, face.style
        ),
        Substitution::Style => format!(
            "{} has no {} on this machine. Umber is setting this text in {} instead. \
             Your choice is kept, so the text goes back to it if the style turns up again.",
            face.family, ed.text.style, face.style
        ),
    };
    ui.add_space(4.0);
    controls::note(ui, p, &body);
}

/// What to tell the artist about a block that would not set.
///
/// **One statement of these five sentences**, because there are two places they
/// have to be said from and two hand-written copies of a notice is how the two
/// come to disagree about what went wrong. The panel draws it under the preview
/// and disables Place with it as the tooltip; `UmberApp::place_text` raises it
/// as a notice, which is the belt to the panel's braces — the gate catches the
/// click, the notice catches a route that goes round the gate.
///
/// [`TextError::Empty`] has an arm here and the panel never asks for it: the
/// preview returns early on an empty block and `place_row` refuses on the same
/// reading, so the panel's answer to "nothing typed" is a disabled button and
/// no sentence at all. `place_text` still needs one.
pub fn refusal(err: TextError) -> String {
    match err {
        TextError::Empty => "Type something into the Text panel first.".to_string(),
        TextError::NoInk => "What is typed makes no mark. It is spaces, or characters \
             this face has no glyph for."
            .to_string(),
        TextError::TooLarge { width, height } => format!(
            "At this size the text would be {width} × {height} pixels, which is \
             more than Umber will rasterise at once. Reduce the size, or the \
             amount of text."
        ),
        TextError::Unreadable => "The font could not be read. It may have been moved or removed \
             since Umber found it; reopen the Text panel to look again."
            .to_string(),
        // Its own sentence rather than the one above, which accuses the
        // typeface. Not reachable from the rails, which is exactly why the
        // wrong sentence would have survived.
        TextError::NotFinite => "The size, line spacing or tracking is not a number. Drag one of \
             the rails in the Text panel to set it again."
            .to_string(),
    }
}

/// A fingerprint of everything that changes the picture.
///
/// A hash rather than a stored copy of the block: the block holds a `String`
/// that can be a paragraph, and cloning it every frame to compare would be an
/// allocation on the drawing path.
///
/// **`Fonts::generation` is in it, and that is the one nobody thinks of.** The
/// family and the style are *names*; the library they resolve against is
/// replaced wholesale when a scan lands and again when the folder preference
/// changes, and the same two names then mean a different file. Without it the
/// panel goes on drawing a picture, a size and a missing-glyph notice made with
/// a face `resolve` no longer answers with, while Place uses the new one — the
/// two disagreeing silently until something else in this list happens to move.
fn preview_key(ed: &Editor) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let b = &ed.text.block;
    b.text.hash(&mut h);
    b.size.to_bits().hash(&mut h);
    b.line_spacing.to_bits().hash(&mut h);
    b.tracking.to_bits().hash(&mut h);
    b.align.hash(&mut h);
    ed.text.family.hash(&mut h);
    ed.text.style.hash(&mut h);
    ed.text.fonts.generation().hash(&mut h);
    ed.text_colour().to_srgb_u8().hash(&mut h);
    h.finish()
}

/// Set the block twice — small for the picture, full size for the figure and
/// the notices — and keep both.
///
/// Twice, and not once scaled, because trimming to the ink is not linear in the
/// size: a 26-pixel `Hxg` and a 400-pixel one do not have the same proportions
/// once the antialiased edge is a smaller share of the mark. The figure has to
/// be what the canvas will actually receive, so it comes from a real setting.
///
/// The small one is the picture because a caption for a 4000-pixel canvas is
/// not something that fits in a 264-point panel, and what the artist is
/// checking here — the face, the weight, the line breaks — all survives being
/// scaled down.
///
/// Called only when [`preview_key`] has moved. See [`Preview`].
fn build_preview(ui: &Ui, ed: &mut Editor, key: u64) -> Preview {
    let mut small = ed.text.block.clone();
    let ratio = PREVIEW_EM / ed.text.block.size.clamp(text::MIN_SIZE, text::MAX_SIZE);
    small.size = PREVIEW_EM;
    small.tracking *= ratio;
    let colour = ed.text_colour();
    let block = ed.text.block.clone();

    // Through the cache, so the font file is read once per face rather than
    // once per keystroke — a CJK collection is sixteen megabytes, and this runs
    // on every character typed.
    let Some((face, data)) = ed.text.face_and_data() else {
        // The same answer `TextState::set` gives when the bytes will not come:
        // a face `resolve` still names whose file has gone since the scan. Said
        // here rather than left blank, or the panel is silent and Place is the
        // first thing to mention it.
        return Preview {
            key,
            picture: None,
            measured: Err(Refused::new(TextError::Unreadable)),
            missing: Vec::new(),
            mixed: false,
        };
    };

    // **Through `Setting::clip`, which is the same call Place makes**, rather
    // than colouring the coverage again here. A second copy of two lines looks
    // harmless and had already drifted: this took `[r, g, b, _]` and used the
    // coverage as the alpha, where `clip` multiplies the coverage by the
    // colour's own alpha — so a colour picked at less than full opacity
    // previewed solid and landed thinner. Nothing reaches a translucent
    // `Editor::color` today, which is exactly why it would have gone on being
    // wrong. A `Clip` holds straight-alpha sRGB, which is what
    // `from_rgba_unmultiplied` wants, so this is a conversion and not an
    // arithmetic of its own.
    let picture = text::set(&face, data, &small).ok().and_then(|setting| {
        let clip = setting.clip(colour)?;
        let pixels: Vec<egui::Color32> = clip
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|px| egui::Color32::from_rgba_unmultiplied(px[0], px[1], px[2], px[3]))
            .collect();
        let image = egui::ColorImage {
            size: [setting.width as usize, setting.height as usize],
            pixels,
            source_size: vec2(setting.width as f32, setting.height as f32),
        };
        Some(
            ui.ctx()
                .load_texture("text-preview", image, egui::TextureOptions::LINEAR),
        )
    });

    // The real block, for the figure and the notices. `missing` and
    // `mixed_directions` are read off *this* setting rather than the small one:
    // the two agree today, and reading them from the picture would make the
    // notice a statement about the preview rather than about what is going on
    // the canvas.
    let (measured, missing, mixed) = match text::set(&face, data, &block) {
        Ok(setting) => (
            Ok((setting.width, setting.height)),
            setting.missing,
            setting.mixed_directions,
        ),
        // Kept, not discarded. See `Preview::measured`.
        Err(err) => (Err(Refused::new(err)), Vec::new(), false),
    };
    Preview {
        key,
        picture,
        measured,
        missing,
        mixed,
    }
}

/// What the block will look like, in the face it will be set in, and why it
/// would not go on the canvas where it would not.
///
/// Returning the refusal is what lets [`place_row`] be right without reading
/// the cache itself: this is the one place that knows whether what is cached
/// describes what is typed *now*, because the empty case returns before
/// rebuilding it.
fn preview(ui: &mut Ui, p: &Palette, ed: &mut Editor) -> Option<TextError> {
    if ed.text.block.text.trim().is_empty() {
        return None;
    }

    let key = preview_key(ed);
    // **Rebuilt only when something that changes it has changed.** Everything
    // below this line rasterises the block twice — once small for the picture
    // and once at its real size for the figure and the notices — and a panel
    // body runs on every frame the module is open. Doing it unconditionally
    // would rasterise somebody's caption sixty times a second for as long as
    // the panel was on screen.
    if ed.text.preview.as_ref().map(|c| c.key) != Some(key) {
        ed.text.preview = Some(build_preview(ui, ed, key));
    }
    let cache = ed.text.preview.as_ref()?;
    let (measured, missing, mixed) = (
        cache.measured.as_ref(),
        cache.missing.as_slice(),
        cache.mixed,
    );

    if let Some(handle) = &cache.picture {
        let [w, h] = handle.size();
        let (w, h) = (w as f32, h as f32);
        let full = ui.available_width();
        let scale = (full / w).min(PREVIEW_MAX_HEIGHT / h).min(1.0);
        let size = vec2(w * scale, h * scale);
        // The plate is the panel's full width whatever the text measures, so a
        // long caption and a short one are the same control rather than a strip
        // that changes size as somebody types.
        let (behind, _) = ui.allocate_exact_size(vec2(full, size.y + 12.0), Sense::hover());
        plate(ui, p, behind, ed.doc.background);
        let rect = egui::Rect::from_center_size(behind.center(), size);
        ui.painter().image(
            handle.id(),
            rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
        ui.add_space(4.0);
    }

    match measured {
        Ok(&(w, h)) => {
            ui.label(
                egui::RichText::new(format!("{w} × {h} px on the canvas"))
                    .size(texttokens::TINY)
                    .color(p.text_dim),
            );
        }
        // **The refusal goes where the measurement would have been.** It is the
        // same slot answering the same question — what happens if I click
        // Place — and the whole failure this replaces was the line simply
        // vanishing while a picture stayed on screen beside a live button.
        //
        // A picture may well still be drawn above it, and that is right: the
        // preview is rasterised at `PREVIEW_EM`, so it is an honest picture of
        // the face and the line breaks even for a block far past the cap. What
        // it cannot say is that the block will not go on the canvas, which is
        // what this says.
        Err(refused) => controls::note(ui, p, &refused.line),
    }

    // Both of these are the import rule applied here: an operation that loses
    // something says so, in a finished sentence, rather than leaving it to be
    // discovered on the canvas.
    if !missing.is_empty() {
        // **By codepoint, never by drawing the characters.** The interface is
        // set in Archivo, so a character *this* face has no glyph for is one
        // Archivo very likely has none for either — printing it here would draw
        // the blank box the "no Unicode symbols in the UI" rule exists to
        // prevent, in the one sentence whose whole job is to say which
        // character is missing. `U+5B57` always renders and is what somebody
        // would search for.
        let list = missing
            .iter()
            .take(6)
            .map(|c| format!("U+{:04X}", *c as u32))
            .collect::<Vec<_>>()
            .join(", ");
        let more = if missing.len() > 6 {
            format!(" and {} more", missing.len() - 6)
        } else {
            String::new()
        };
        controls::note(
            ui,
            p,
            &format!(
                "This face has no glyph for {list}{more}. They are left blank \
                 rather than drawn as a box. Choose another font, or remove them."
            ),
        );
    }
    if mixed {
        controls::note(
            ui,
            p,
            "This line mixes left-to-right and right-to-left writing. Umber \
             does not reorder the two yet, so they may come out in the wrong \
             order.",
        );
    }
    measured.err().map(|refused| refused.err)
}

/// What the preview sits on: **this document's own background**.
///
/// Not the panel's fill and not a plate chosen to make the preview legible.
/// The question the preview answers is "what will this look like when I put it
/// down", and the honest answer to that is the paint over the canvas it is
/// going onto — so a white-backed document shows dark text on white, exactly
/// as it will land, and a transparent one shows the checker, which is what the
/// canvas itself draws and what a layer thumbnail draws.
///
/// The alternative — brightening the plate, or the ink, so that the preview
/// always reads — would be showing a colour nobody chose. The Colour panel is
/// where the colour is and where to change it.
///
/// The checker is drawn on the transparent case rather than a flat fill for the
/// reason the layer thumbnail gives: what is being previewed is pixels *with
/// alpha*, and a flat fill would say the text was opaque where it is
/// antialiased.
fn plate(ui: &Ui, p: &Palette, rect: egui::Rect, background: Background) {
    const CELL: f32 = 6.0;
    let painter = ui.painter();
    if let Background::Colour(colour) = background {
        let [r, g, b, _] = colour.to_srgb_u8();
        painter.rect_filled(rect, metrics::RADIUS, egui::Color32::from_rgb(r, g, b));
        return;
    }
    painter.rect_filled(rect, metrics::RADIUS, p.window);
    let cols = (rect.width() / CELL).ceil() as usize;
    let rows = (rect.height() / CELL).ceil() as usize;
    for i in 0..cols {
        for j in 0..rows {
            if (i + j) % 2 == 0 {
                continue;
            }
            let cell = egui::Rect::from_min_size(
                rect.left_top() + vec2(i as f32 * CELL, j as f32 * CELL),
                vec2(CELL, CELL),
            );
            painter.rect_filled(cell.intersect(rect), 0.0, p.control_hover);
        }
    }
}

/// The Place button, and why it might be refused.
///
/// Disabled rather than live-and-then-a-dialog where the answer is already
/// known — the rule "Clear layer" and the selection's Cut both follow. The lock
/// and the folder are still gated for real in `begin_float`, so a shortcut
/// cannot go round this.
///
/// **The block itself is one of the answers already known**, and it used not to
/// be read here. `build_preview` sets the real block at its real size on the
/// way to drawing the panel, so the exact [`TextError`] is sitting in the cache
/// by the time this row is drawn; a live button over it is precisely the
/// dialog-after-the-click this rule exists to prevent, and past
/// `text::MAX_PIXELS` it was a live button under a preview picture that had
/// rasterised perfectly well at [`PREVIEW_EM`].
fn place_row(
    ui: &mut Ui,
    p: &Palette,
    ed: &Editor,
    refused: Option<TextError>,
    actions: &mut UiActions,
) {
    // Read before the toggle is drawn, so the button and the switch describe the
    // same frame: a toggle applied first would light Place on the strength of a
    // layer this frame's readings say nothing about.
    let state = place_state(
        Landing::of(ed),
        ed.text.block.text.trim().is_empty(),
        refused,
    );
    let mut own = ed.ui.text_own_layer;
    let row = widgets::toggle_row(ui, p, "On its own layer", &mut own);
    if own != ed.ui.text_own_layer {
        // Collected rather than written, for the reason every other control in
        // this panel is collected: the panel holds `&Editor` and `app.rs` is the
        // one writer, so a frame cannot draw one answer and act on another.
        actions.text_own_layer = Some(own);
    }
    row.on_hover_text(
        "Each placement makes a layer named after its own words. That is what \
         keeps text editable: Umber can only offer to set it again where it can \
         tell the words from the picture underneath.",
    );
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let response = controls::text_button(ui, p, "Place", true, state.enabled);
            if response.clicked() {
                actions.place_text = true;
            }
            response.on_hover_text(state.tooltip.as_ref());
        });
    });
}

/// Where a placement would land, and what stands in its way.
///
/// **Which readings even apply depends on the mode**, which is the whole reason
/// this is a struct rather than three more positional booleans. A placement that
/// makes its own layer is not gated by the selected layer's lock at all — the
/// new layer carries no lock — and it *is* gated by whether the stack has room,
/// which a placement onto the selected layer never is. Handing
/// [`place_state`] one set of flags whose meaning silently changed with a fourth
/// would be the partially-exhaustive reading this codebase records elsewhere,
/// wearing a boolean's clothes.
#[derive(Clone, Copy, Debug)]
struct Landing {
    /// `Editor::ui.text_own_layer`.
    own_layer: bool,
    /// Is the place the text would go locked?
    ///
    /// The *place*, not the selected entry: with `own_layer` on this is
    /// `LayerStack::new_layer_would_be_locked`, which asks about what would
    /// enclose the new layer, and with it off it is the selected layer's own
    /// effective lock. See that method for why the two differ.
    locked: bool,
    /// A folder is selected and the text would have to land on it. Only
    /// reachable with `own_layer` off: with it on the new layer goes *inside*
    /// the folder, which is where every application puts one.
    folder: bool,
    /// There is nowhere to put a new layer — the stack is at
    /// `LayerStack::MAX`, or the selected folder is nested as deep as it goes.
    /// Only reachable with `own_layer` on.
    no_room: bool,
}

impl Landing {
    fn of(ed: &Editor) -> Self {
        let own_layer = ed.ui.text_own_layer;
        let at_depth = ed
            .layers
            .get(ed.layers.active_index())
            .is_some_and(|l| l.is_folder() && l.depth >= umber_core::LayerStack::MAX_DEPTH);
        Self {
            own_layer,
            locked: if own_layer {
                ed.layers.new_layer_would_be_locked()
            } else {
                ed.layers.active_is_locked()
            },
            folder: !own_layer && ed.layers.active_slot().is_none(),
            no_room: own_layer && (ed.layers.len() >= umber_core::LayerStack::MAX || at_depth),
        }
    }
}

/// Whether Place may be pressed, and what to say about it.
///
/// A pure function of its readings, and separate from [`place_row`] for the
/// reason `gesture::press` and `dock`'s drop rules are separate from what draws
/// them: this is the whole of the decision, and the decision is the part that
/// was wrong. Reading it out of a running window is not a test anybody can run
/// on CI.
struct Place {
    enabled: bool,
    tooltip: Cow<'static, str>,
}

/// See [`Place`].
///
/// The order of the refusals is deliberate. The ones about *where* the text
/// would go come first, because somebody whose layer is locked is not helped by
/// also being told about the size; `empty` comes before the block's own refusal
/// because the preview does not rebuild for an empty block, so `refused` may be
/// describing what was there a keystroke ago.
fn place_state(landing: Landing, empty: bool, refused: Option<TextError>) -> Place {
    if landing.locked {
        return Place {
            enabled: false,
            // Two sentences, because with a layer of its own the lock the artist
            // has to find is not the one on the row they are looking at: it is
            // on a folder somewhere above it, and "the layer is locked" would
            // send them to unlock a layer that is not the problem.
            tooltip: if landing.own_layer {
                "The folder this would go in is locked. Unlock it in the Layers panel, \
                 or select a layer outside it."
                    .into()
            } else {
                "The layer is locked. Unlock it in the Layers panel, or select another.".into()
            },
        };
    }
    if landing.folder {
        return Place {
            enabled: false,
            tooltip: "A folder is selected. A folder holds no pixels, so select a layer, \
                      or switch on \"On its own layer\"."
                .into(),
        };
    }
    if landing.no_room {
        return Place {
            enabled: false,
            tooltip: "There is nowhere to put another layer. Delete one, or switch off \
                      \"On its own layer\" to set the words on the layer that is selected."
                .into(),
        };
    }
    if empty {
        return Place {
            enabled: false,
            tooltip: "Type something first.".into(),
        };
    }
    if let Some(err) = refused {
        // The same sentence the note under the preview draws, from the same
        // function, so the tooltip and the panel cannot say different things
        // about one block.
        return Place {
            enabled: false,
            tooltip: refusal(err).into(),
        };
    }
    Place {
        enabled: true,
        // It names *where* as well as what, because that is the half the switch
        // above it changes and a button whose outcome depends on a control
        // beside it should say which way that control is set.
        tooltip: if landing.own_layer {
            "Put the text on a layer of its own, where the transform tool can move, \
             scale and turn it before it is committed"
                .into()
        } else {
            "Put the text on the selected layer, where the transform tool can move, \
             scale and turn it before it is committed"
                .into()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;

    /// The panel is usable on the first frame and on a machine whose scan finds
    /// nothing: the interface's own face is in the library before any thread
    /// has run, and the default family and style name it.
    #[test]
    fn the_panel_can_set_text_before_any_scan_has_run() {
        let ed = Editor::default();
        assert!(!ed.text.fonts.scanning());
        assert!(!ed.text.fonts.library().is_empty());
        let face = ed.text.face().expect("a face before the scan");
        assert_eq!(face.family, ed.text.family);
    }

    /// What Place puts down is an ordinary clip in the artist's own colour with
    /// the coverage as its alpha — which is exactly what `float_a_clip` hands
    /// to `Clip::place` for a paste, and the whole of why nothing new reaches
    /// the GPU.
    #[test]
    fn what_place_would_put_down_is_an_ordinary_clip() {
        let mut ed = Editor::default();
        ed.text.block.text = "Umber".to_string();
        ed.text.block.size = 64.0;
        let setting = ed.text.set().expect("ink");
        let clip = setting.clip(ed.color).expect("a clip");
        assert_eq!(clip.size().x, setting.width);
        assert_eq!(clip.size().y, setting.height);
        assert!(clip.pixels().iter().skip(3).step_by(4).any(|&a| a > 200));
    }

    /// Nothing typed is refused rather than producing an empty float, and it is
    /// told apart from a line of spaces — the notice has different sentences
    /// for the two.
    #[test]
    fn an_empty_block_places_nothing() {
        let mut ed = Editor::default();
        assert_eq!(ed.text.set().err(), Some(TextError::Empty));
        ed.text.block.text = "   ".to_string();
        assert_eq!(ed.text.set().err(), Some(TextError::NoInk));
    }

    /// A family the machine does not have still resolves, because a preference
    /// records names and the machine it is read back on may have neither. The
    /// panel would otherwise be a picker that cannot draw anything until
    /// somebody works out why.
    #[test]
    fn a_face_that_is_not_here_still_sets_something() {
        let mut ed = Editor::default();
        ed.text.family = "A Foundry Face Nobody Has".to_string();
        ed.text.style = "Ultra Condensed Black Italic".to_string();
        ed.text.block.text = "Umber".to_string();
        assert!(ed.text.set().is_ok());
    }

    /// The preview is keyed by everything that changes the picture — including
    /// the colour, which is the one to forget, because it is not in the block.
    /// A key that missed one would draw a preview of a caption somebody had
    /// already changed, which is the control that lies at its smallest.
    #[test]
    fn the_preview_key_moves_with_everything_that_changes_the_picture() {
        let typed = |f: fn(&mut Editor)| {
            let mut ed = Editor::default();
            ed.text.block.text = "Umber".to_string();
            f(&mut ed);
            preview_key(&ed)
        };
        let base = typed(|_| {});
        assert_eq!(base, typed(|_| {}), "the key is not stable");

        for (what, f) in [
            (
                "text",
                (|ed: &mut Editor| ed.text.block.text.push('s')) as fn(&mut Editor),
            ),
            ("size", |ed| ed.text.block.size += 1.0),
            ("line spacing", |ed| ed.text.block.line_spacing += 0.1),
            ("tracking", |ed| ed.text.block.tracking += 1.0),
            ("align", |ed| ed.text.block.align = Align::Right),
            ("family", |ed| ed.text.family = "Other".to_string()),
            ("style", |ed| ed.text.style = "Bold".to_string()),
            ("colour", |ed| {
                ed.color = umber_core::Color::from_srgb_u8(200, 30, 30, 255)
            }),
            // The one nobody thinks of. The family and the style are *names*,
            // and the library they resolve against is replaced wholesale when a
            // scan lands or the folder preference changes — so the same two
            // names then mean a different file. Without this the panel goes on
            // drawing a picture, a size and a missing-glyph notice made with a
            // face `resolve` no longer answers with, while Place uses the new
            // one.
            ("font library", |ed| ed.text.fonts.forget()),
        ] {
            assert_ne!(typed(f), base, "changing the {what} did not move the key");
        }
    }

    /// A face is read off the disk once per face, not once per keystroke.
    ///
    /// A font file is read *whole* and a CJK collection is sixteen megabytes;
    /// the preview is rebuilt on every character typed, so a read there would
    /// put a disk hit and a full parse between somebody and each key they
    /// press. The cache has to survive the block changing and must **not**
    /// survive the library changing, because the same two names then name a
    /// different file.
    #[test]
    fn the_font_file_is_read_once_per_face_and_not_once_per_keystroke() {
        let mut ed = Editor::default();
        ed.text.block.text = "U".to_string();
        assert!(ed.text.set().is_ok());
        let first = ed
            .text
            .loaded
            .as_ref()
            .map(|(f, s, g, _)| (f.clone(), s.clone(), *g));
        assert!(first.is_some(), "nothing was cached");

        // Typing more does not re-read.
        ed.text.block.text.push('m');
        assert!(ed.text.set().is_ok());
        assert_eq!(
            ed.text
                .loaded
                .as_ref()
                .map(|(f, s, g, _)| (f.clone(), s.clone(), *g)),
            first,
            "the same face was read again"
        );

        // A new library does.
        ed.text.fonts.forget();
        assert!(ed.text.set().is_ok());
        let after = ed
            .text
            .loaded
            .as_ref()
            .map(|(f, s, g, _)| (f.clone(), s.clone(), *g));
        assert_ne!(after, first, "a replaced library kept the old face's bytes");
    }

    /// A block the engine will refuse is a refusal the panel can state, for
    /// every error there is.
    ///
    /// The panel's job here is to know *before* the click, and the only way it
    /// can is by keeping what `build_preview` already found out.
    ///
    /// **What forces a sentence for a new variant is `refusal`'s wildcard-free
    /// `match`, not this array**, which is hand-written and would happily go on
    /// passing. This checks the shape of what comes out: a finished sentence
    /// rather than a code, and no em-dash, which is the house rule for anything
    /// the interface draws.
    #[test]
    fn every_reason_a_block_will_not_set_has_a_finished_sentence() {
        for err in [
            TextError::Empty,
            TextError::NoInk,
            TextError::TooLarge {
                width: 9000,
                height: 9000,
            },
            TextError::Unreadable,
            TextError::NotFinite,
        ] {
            let line = refusal(err);
            assert!(line.ends_with('.'), "{err:?} is not a sentence: {line:?}");
            assert!(
                line.len() > 20,
                "{err:?} is a code, not a sentence: {line:?}"
            );
            // No em-dash in anything the interface draws.
            assert!(!line.contains('—'), "{err:?} carries an em-dash: {line:?}");
        }
        // The one that names what was asked for rather than saying "too big".
        let line = refusal(TextError::TooLarge {
            width: 9000,
            height: 4000,
        });
        assert!(line.contains("9000") && line.contains("4000"), "{line:?}");
    }

    /// Every sentence the real panel body draws, for a block in a given state.
    ///
    /// The panel is run headlessly through `Context::run_ui` and its text is
    /// read back off the shapes it emitted, which is the only way to assert
    /// that a notice is *drawn* rather than merely that some function would
    /// return it. Without this, deleting the `controls::note` call under the
    /// preview or the `substitution_note` call altogether leaves every test in
    /// the workspace green — which is exactly what a critic found, and it was
    /// true of the two fixes this whole branch exists for.
    ///
    /// Twice, because the first pass through a fresh context builds the font
    /// atlas: `panels.rs`'s `ticking_a_layer_does_not_move_the_layer_list`
    /// takes the second reading for the same reason.
    fn panel_text(prepare: impl Fn(&mut Editor)) -> String {
        use egui::{Rect, pos2, vec2};

        fn collect(shape: &egui::Shape, out: &mut String) {
            match shape {
                egui::Shape::Text(text) => {
                    out.push_str(text.galley.text());
                    out.push('\n');
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, out);
                    }
                }
                _ => {}
            }
        }

        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                pos2(0.0, 0.0),
                vec2(crate::theme::metrics::PANEL, 900.0),
            )),
            ..Default::default()
        };
        let mut ed = Editor::default();
        // No scan: it would spawn a thread per pass and, worse, a machine that
        // actually has the family a case asks for would draw a different panel.
        ed.text.fonts.hold_at_builtin();
        prepare(&mut ed);
        let palette = crate::theme::Palette::of(ed.ui.theme);
        let mut text = String::new();
        for _ in 0..2 {
            text.clear();
            let output = ctx.run_ui(input.clone(), |ui| {
                let mut actions = UiActions::default();
                panel(ui, &palette, &mut ed, &mut actions);
            });
            for clipped in &output.shapes {
                collect(&clipped.shape, &mut text);
            }
        }
        text
    }

    /// The panel says why a block will not go on the canvas, in the sentence
    /// `place_text` would have raised after the click.
    ///
    /// The failure this pins is specific and was live: a block past
    /// `text::MAX_PIXELS` drew a preview picture, because the preview
    /// rasterises at `PREVIEW_EM` and 26 pixels succeeds where 1000 does not,
    /// and then silently dropped the size line and left Place enabled.
    #[test]
    fn the_panel_draws_the_refusal_for_a_block_that_will_not_set() {
        let ordinary = panel_text(|ed| {
            ed.text.block.text = "Umber".to_string();
            ed.text.block.size = 72.0;
        });
        assert!(
            ordinary.contains("px on the canvas"),
            "no size line on an ordinary block: {ordinary}"
        );
        assert!(
            !ordinary.contains("more than Umber will rasterise"),
            "an ordinary block was refused: {ordinary}"
        );

        // **The fixture had to grow, and that is the change working rather than
        // the test rotting.** One line of this at `MAX_SIZE` used to clear
        // `text::MAX_PIXELS` because `set` padded by a whole em on every side
        // without knowing how far an outline actually strays past its advance
        // width. Measuring the block through its own transform took that away —
        // 2.6x the buffer at identity — so the same caption now *fits*, and the
        // panel was right to stop refusing it.
        //
        // Several lines rather than a longer line, because width alone runs into
        // the coordinate ceiling before the area cap and would be refused for the
        // wrong reason. What is being pinned is the *area* branch.
        let past_the_cap = panel_text(|ed| {
            ed.text.block.text = std::iter::repeat_n("A caption nobody could fit on a canvas", 6)
                .collect::<Vec<_>>()
                .join("\n");
            ed.text.block.size = text::MAX_SIZE;
        });
        assert!(
            past_the_cap.contains("more than Umber will rasterise at once"),
            "the panel said nothing about a block past the cap: {past_the_cap}"
        );
        assert!(
            !past_the_cap.contains("px on the canvas"),
            "a refused block still claimed a size: {past_the_cap}"
        );
    }

    /// The panel names a substituted face, and stays quiet when there is none.
    ///
    /// `FontLibrary::resolve` is total, so without this sentence the dropdown
    /// shows one family while the preview, the measurement and Place all use
    /// another.
    #[test]
    fn the_panel_names_a_face_it_had_to_substitute() {
        let honest = panel_text(|ed| ed.text.block.text = "Umber".to_string());
        assert!(
            !honest.contains("is not on this machine"),
            "a face that resolved exactly was reported as substituted: {honest}"
        );

        // A name no scan can find, so this reads the same on every machine —
        // "Helvetica Neue" would be installed on any Mac and the case would
        // quietly stop testing anything there.
        let substituted = panel_text(|ed| {
            ed.text.block.text = "Umber".to_string();
            ed.text.family = "A Foundry Face Nobody Has".to_string();
        });
        assert!(
            substituted.contains("A Foundry Face Nobody Has is not on this machine"),
            "the substitution was silent: {substituted}"
        );
        assert!(
            substituted.contains("Archivo"),
            "the substitute was not named: {substituted}"
        );
    }

    /// Place is offered exactly when the block will actually go down.
    ///
    /// Every cell of the matrix, because the defect was one missing conjunct:
    /// the button was live over a block the engine had already refused, and
    /// the only way to find out was to click it.
    #[test]
    fn place_is_refused_for_every_reason_it_should_be() {
        let too_large = TextError::TooLarge {
            width: 9000,
            height: 9000,
        };
        // The whole matrix is swept **in both modes**, because which readings
        // even apply changes with the mode: `folder` is unreachable with a layer
        // of its own and `no_room` is unreachable without one, and a sweep in
        // one mode alone would be testing whichever half it happened to pick.
        for own_layer in [false, true] {
            let clear = Landing {
                own_layer,
                locked: false,
                folder: false,
                no_room: false,
            };
            for (landing, why) in [
                (
                    Landing {
                        locked: true,
                        ..clear
                    },
                    "locked",
                ),
                (
                    Landing {
                        folder: true,
                        ..clear
                    },
                    "a folder",
                ),
                (
                    Landing {
                        no_room: true,
                        ..clear
                    },
                    "no room",
                ),
            ] {
                let state = place_state(landing, false, None);
                assert!(!state.enabled, "Place was live for {why} ({own_layer})");
                assert!(!state.tooltip.is_empty());
            }
            for refused in [
                Some(too_large),
                Some(TextError::NoInk),
                Some(TextError::Unreadable),
            ] {
                assert!(!place_state(clear, false, refused).enabled);
            }
            assert!(!place_state(clear, true, None).enabled, "an empty block");

            // And the one case that must be live.
            let state = place_state(clear, false, None);
            assert!(state.enabled, "Place was refused with nothing wrong");
            assert!(state.tooltip.contains("Put the text on"));

            // The refusal's tooltip is the note's sentence, from `refusal`, so
            // the two cannot say different things about one block.
            assert_eq!(
                place_state(clear, false, Some(too_large)).tooltip,
                refusal(too_large)
            );
            // A lock outranks the size: the artist is not helped by being told
            // about both, and the layer is the thing to fix first.
            let locked = Landing {
                locked: true,
                ..clear
            };
            assert_eq!(
                place_state(locked, false, Some(too_large)).tooltip,
                place_state(locked, false, None).tooltip
            );
        }
    }

    /// The button says where the text will go, and the two answers differ.
    ///
    /// A tooltip that read the same either way would be a control whose outcome
    /// depends on a switch beside it saying nothing about which way that switch
    /// is set — and the *lock* refusal has the same shape, sending somebody to
    /// unlock the row they are looking at when the lock is on a folder above it.
    #[test]
    fn place_says_which_layer_the_text_is_going_on() {
        let own = Landing {
            own_layer: true,
            locked: false,
            folder: false,
            no_room: false,
        };
        let here = Landing {
            own_layer: false,
            ..own
        };
        assert_ne!(
            place_state(own, false, None).tooltip,
            place_state(here, false, None).tooltip,
            "the button promised the same thing in both modes"
        );
        assert!(place_state(own, false, None).tooltip.contains("its own"));
        assert!(place_state(here, false, None).tooltip.contains("selected"));

        let locked_own = Landing { locked: true, ..own };
        let locked_here = Landing {
            locked: true,
            ..here
        };
        assert!(
            locked_own.locked && locked_here.locked,
            "both readings are of a locked landing"
        );
        assert!(
            place_state(locked_own, false, None)
                .tooltip
                .contains("folder"),
            "with a layer of its own, the lock to find is on a folder"
        );
        assert!(
            place_state(locked_here, false, None)
                .tooltip
                .contains("The layer is locked"),
        );
    }

    /// The two readings of "is the landing locked" disagree, and the panel takes
    /// the one that matches what the placement will do.
    ///
    /// This is the guard on the *panel* rather than on the model: deleting the
    /// `own_layer` branch of `Landing::of` and reading `active_is_locked` in
    /// both leaves `only_what_encloses_a_new_layer_can_lock_it` perfectly green,
    /// because that one drives `LayerStack` and cannot see what calls it.
    #[test]
    fn the_panel_reads_the_lock_that_belongs_to_the_mode() {
        let mut ed = Editor::default();
        ed.layers.add();
        // The selected layer is locked; nothing encloses it.
        let at = ed.layers.active_index();
        ed.layers.get_mut(at).unwrap().locked = true;

        ed.ui.text_own_layer = false;
        assert!(
            Landing::of(&ed).locked,
            "placing onto the locked layer itself is refused"
        );
        ed.ui.text_own_layer = true;
        assert!(
            !Landing::of(&ed).locked,
            "a new layer beside a locked one is not itself locked"
        );
    }

    /// A block past the cap **draws a picture and still refuses**, and the
    /// panel knows which before anybody clicks.
    ///
    /// This is the defect in one test. The preview rasterises at `PREVIEW_EM`,
    /// which is 26 pixels and succeeds where the real size does not, so the
    /// artist saw a perfectly good picture of their caption; the measurement
    /// line silently vanished, Place stayed live, and the first news of the
    /// refusal was a notice after the click.
    #[test]
    fn a_block_past_the_cap_is_refused_where_the_panel_can_say_so() {
        let mut ed = Editor::default();
        ed.text.block.text = "M".repeat(4000);
        ed.text.block.size = text::MAX_SIZE;

        // The engine refuses it, which is what the panel has to be able to
        // report rather than discover.
        let err = ed.text.set().expect_err("past the cap");
        assert!(matches!(err, TextError::TooLarge { .. }), "{err:?}");

        // And the small preview genuinely does set, which is why the panel used
        // to draw a picture and say nothing at all.
        let mut small = ed.text.block.clone();
        small.size = PREVIEW_EM;
        let (face, data) = {
            let face = ed.text.face().expect("a face").clone();
            let data = face.load().expect("bytes");
            (face, data)
        };
        assert!(
            text::set(&face, &data, &small).is_ok(),
            "the preview no longer draws a picture for a block past the cap, \
             so this test is guarding nothing"
        );
    }

    /// A face that had to be substituted is named, and one that did not is not.
    ///
    /// `resolve` is total, so the panel is the only thing that can say a
    /// substitution happened — and it did not. Reachable because the library is
    /// replaced wholesale when a scan lands or the folder preference changes,
    /// which is what `Fonts::forget` is here to stand in for.
    #[test]
    fn a_family_this_machine_does_not_have_is_named_rather_than_swapped_silently() {
        let ed = Editor::default();
        let library = ed.text.fonts.library();
        // The default pair is real, so the panel stays quiet.
        assert!(
            library
                .substituted(&ed.text.family, &ed.text.style)
                .is_none()
        );

        assert_eq!(
            library
                .substituted("A Foundry Face Nobody Has", "Regular")
                .map(|(what, _)| what),
            Some(Substitution::Family)
        );
        assert_eq!(
            library
                .substituted("Archivo", "Ultra Condensed Black Italic")
                .map(|(what, _)| what),
            Some(Substitution::Style)
        );

        // And the artist's choice is *kept*, not rewritten to what resolved.
        // Rewriting is the tempting repair and it throws away the ability to
        // get the face back when the font turns up again.
        let mut ed = Editor::default();
        ed.text.family = "A Foundry Face Nobody Has".to_string();
        ed.text.block.text = "Umber".to_string();
        assert!(ed.text.set().is_ok(), "it should still set in something");
        assert_eq!(
            ed.text.family, "A Foundry Face Nobody Has",
            "the picker rewrote itself to the substitute"
        );
    }

    /// The family filter folds case and matches what the menu will show.
    ///
    /// Both halves read the same predicate, so the figure on the trigger cannot
    /// promise a number of rows the list then does not draw — and neither of
    /// them lowers a copy of anything. See `widgets::contains_ignore_case`.
    #[test]
    fn the_font_search_folds_case_and_the_figure_matches_the_list() {
        let ed = Editor::default();
        let library = ed.text.fonts.library();
        let matching = |query: &str| {
            library
                .families_iter()
                .filter(|name| widgets::contains_ignore_case(name, query))
                .count()
        };
        // The shipped library is Archivo alone, in whatever capitals the font
        // states, so every fold of it has to find the one family.
        for query in ["Archivo", "archivo", "ARCHIVO", "chi"] {
            assert_eq!(matching(query), 1, "{query}");
        }
        assert_eq!(matching("helvetica"), 0);
        // An empty query is not a filter, so the figure falls back to the
        // cached count rather than being recomputed. **The two have to be the
        // same quantity**, which they were not: the cached one counted faces
        // and the filtered one counts families, so typing a character took the
        // trigger from the number of styles to the number of typefaces.
        assert_eq!(matching(""), library.families().len());
        assert_eq!(
            ed.text.fonts.family_count(),
            library.families().len().to_string(),
            "the unfiltered figure counts something other than the filtered one"
        );
    }

    /// **Pressing Bold changes the picture.**
    ///
    /// Measured by rasterising, twice, through the panel's own path — not by
    /// reading a weight off a struct, which is what would pass while the
    /// location was being dropped on the floor somewhere between here and
    /// `skrifa`. Archivo carries nine weights in one file, so its bold is a
    /// variable *instance* rather than a second file: if the axes were being
    /// ignored the two settings would come out byte for byte identical, which is
    /// the failure `cputext.rs` exists to avoid on the splash and the one the
    /// interface's own text still has.
    ///
    /// Ink rather than a pixel: a heavier weight puts more coverage down over
    /// the same word, which is a statement about the mark rather than about
    /// where the rasteriser happened to put an edge.
    #[test]
    fn pressing_bold_actually_puts_a_heavier_mark_on_the_canvas() {
        let ink = |ed: &mut Editor| -> u64 {
            ed.text
                .set()
                .expect("ink")
                .coverage
                .iter()
                .map(|&c| c as u64)
                .sum()
        };

        let mut ed = Editor::default();
        ed.text.fonts.hold_at_builtin();
        ed.text.block.text = "UMBER".to_string();
        ed.text.block.size = 64.0;
        let regular = ink(&mut ed);
        let was = ed.text.style.clone();

        assert!(
            apply_emphasis(&mut ed, Emphasis::Bold),
            "Archivo has a bold and it was refused"
        );
        assert_ne!(ed.text.style, was, "the style name did not move");
        let was_bold = ed.text.style.clone();
        let bold = ink(&mut ed);
        assert!(
            bold > regular * 5 / 4,
            "bold ({bold}) is not meaningfully heavier than regular ({regular})"
        );

        // The face it landed on is one the picker lists, never a name made up
        // here, and it really is heavy.
        let face = ed.text.face().expect("a face").clone();
        assert!(face.is_bold(), "{face:?}");
        assert!(
            ed.text
                .fonts
                .library()
                .exact(&ed.text.family, &ed.text.style)
                .is_some(),
            "the mark left the panel naming a style the library does not have"
        );
        // **And the mark now reads as on**, which is the half nothing asserted:
        // forcing `Toggle::on` to `false` left the whole suite green, so a chip
        // that never lit was untested.
        let lit = emphasis(
            ed.text.fonts.library(),
            &ed.text.family,
            &ed.text.style,
            Emphasis::Bold,
        );
        assert!(lit.on, "the mark did not light on the face it just chose");

        // And it comes back off, onto a real lighter face, with a lighter mark.
        assert!(apply_emphasis(&mut ed, Emphasis::Bold));
        assert!(!ed.text.face().expect("a face").is_bold());
        assert!(ink(&mut ed) < bold);
        assert!(
            !emphasis(
                ed.text.fonts.library(),
                &ed.text.family,
                &ed.text.style,
                Emphasis::Bold
            )
            .on
        );

        // **From a face on the bold side that is not the family's bold**, which
        // is the case the whole rework exists for and the one nothing here could
        // see. `Face::is_bold` and `FontLibrary::is_bold_anchor` agree on
        // Regular, so every assertion above passes under either reading; a
        // critic reverted this panel's call site to `is_bold` and all 1,485
        // tests stayed green. Archivo has four upright faces at or above 600, so
        // there is a real one to start from.
        let heavy = ed
            .text
            .fonts
            .library()
            .family(&ed.text.family)
            .into_iter()
            .filter(|f| f.is_bold() && !f.italic)
            .map(|f| (f.weight, f.style.clone()))
            .min()
            .expect("a face on the bold side");
        assert!(
            ed.text
                .fonts
                .library()
                .family(&ed.text.family)
                .iter()
                .filter(|f| f.is_bold() && !f.italic)
                .count()
                > 1,
            "one face on the bold side, so this case cannot arise and guards nothing"
        );
        ed.text.style = heavy.1.clone();
        let from_heavy = ink(&mut ed);
        let mark = emphasis(
            ed.text.fonts.library(),
            &ed.text.family,
            &ed.text.style,
            Emphasis::Bold,
        );
        assert!(
            !mark.on,
            "{} is on the bold side but is not the family's bold, so the mark \
             must read as off or a press will ask for the regular weight",
            heavy.1
        );
        assert!(mark.enabled(), "{} cannot reach the family's bold", heavy.1);
        assert!(apply_emphasis(&mut ed, Emphasis::Bold));
        assert!(
            ink(&mut ed) > from_heavy,
            "pressing Bold on {} made a lighter mark, so it went down a weight",
            heavy.1
        );
        assert_eq!(
            ed.text.style, was_bold,
            "it did not land on the same face Regular's press did"
        );
    }

    /// **The variation location is what makes bold bold**, and this is the
    /// mutation written as a test.
    ///
    /// Archivo's bold is a named *instance* of one file, so the only thing
    /// separating it from Regular is `Face::variations`. Emptying that is exactly
    /// what a `text::set` ignoring the location would amount to, and the whole
    /// point of the check above is that it cannot pass in that case — so this
    /// performs the mutation on the value rather than trusting the reasoning.
    /// The cleared face rasterises as the file's *default master*, and Archivo's
    /// is **SemiBold** rather than Regular, which bounds what can honestly be
    /// asserted here: that clearing the location changes the mark at all. Not
    /// that it stops being bold, because SemiBold is itself on the bold side of
    /// `BOLD_THRESHOLD`; an earlier draft of this comment claimed both.
    #[test]
    fn clearing_a_faces_variations_takes_its_weight_away() {
        let mut ed = Editor::default();
        ed.text.fonts.hold_at_builtin();
        ed.text.block.text = "UMBER".to_string();
        ed.text.block.size = 64.0;
        let block = ed.text.block.clone();

        let bold = ed
            .text
            .fonts
            .library()
            .restyle(&ed.text.family, &ed.text.style, true, false)
            .expect("a bold")
            .clone();
        assert!(
            !bold.variations.is_empty(),
            "the bold is not a variable instance, so this test guards nothing"
        );

        let ink = |face: &umber_core::fonts::Face| -> u64 {
            let data = face.load().expect("bytes");
            text::set(face, &data, &block)
                .expect("ink")
                .coverage
                .iter()
                .map(|&c| c as u64)
                .sum::<u64>()
        };
        let mut flattened = bold.clone();
        flattened.variations.clear();
        assert_ne!(
            ink(&bold),
            ink(&flattened),
            "the location is being ignored: the bold instance and the default \
             master rasterise identically"
        );
    }

    /// The italic mark is refused for a family that has none, and refusing is
    /// the whole feature: the alternative is shearing the upright outlines, which
    /// this codebase will not do. See `FontLibrary::restyle`.
    ///
    /// The shipped library is Archivo's one upright file, so this is the state
    /// the panel opens in on a machine whose scan has not landed.
    #[test]
    fn the_italic_mark_is_refused_rather_than_shearing_an_upright_face() {
        let mut ed = Editor::default();
        ed.text.fonts.hold_at_builtin();
        let before = ed.text.style.clone();

        let italic = emphasis(
            ed.text.fonts.library(),
            &ed.text.family,
            &ed.text.style,
            Emphasis::Italic,
        );
        assert_eq!(
            italic.lacking,
            Some(Lacking::Any),
            "an italic was offered for a family with none, or refused for the wrong reason"
        );
        assert!(!italic.on);
        assert!(
            italic.tip.contains("no italic"),
            "the refusal does not say why: {}",
            italic.tip
        );

        // Bold is offered on the same family, which is what makes the pair a
        // reading of the library rather than a control that is simply off.
        let bold = emphasis(
            ed.text.fonts.library(),
            &ed.text.family,
            &ed.text.style,
            Emphasis::Bold,
        );
        assert!(bold.enabled(), "Archivo's bold was refused");

        // And the refused mark changes nothing if it is reached anyway.
        assert!(!apply_emphasis(&mut ed, Emphasis::Italic));
        assert_eq!(ed.text.style, before);
    }

    /// **A refusal names what is actually missing**, and the four reasons are
    /// four different sentences because they send somebody four different places.
    ///
    /// `can_restyle` answers about this slant at this weight, and the sentences
    /// were written as though it answered about the whole family: Bold on an
    /// italic face, in a family with a bold and no bold italic, read "this family
    /// has no bold on this machine. Install the family's bold weight" with that
    /// bold two rows above in the list beside it.
    #[test]
    fn a_refused_mark_names_what_is_actually_missing() {
        use umber_core::fonts::Source;
        let face = |family: &str, style: &str, weight: u16, italic: bool| Face {
            family: family.to_string(),
            style: style.to_string(),
            weight,
            italic,
            source: Source::File {
                path: std::path::PathBuf::from(format!("{family}-{style}.ttf")),
                index: 0,
            },
            variations: Vec::new(),
        };
        // A library assembled by hand, because these four states need families
        // the shipped font cannot produce.
        let library = |faces: &[Face]| {
            let mut lib = FontLibrary::default();
            for f in faces {
                lib.add_face(f.clone());
            }
            lib
        };

        // Regular, Bold, Italic and no Bold Italic: pressing Bold on the italic
        // is `Lacking::Pairing`, and it must not claim the family has no bold.
        let mixed = library(&[
            face("Foo", "Regular", 400, false),
            face("Foo", "Bold", 700, false),
            face("Foo", "Italic", 400, true),
        ]);
        let bold_on_italic = emphasis(&mixed, "Foo", "Italic", Emphasis::Bold);
        assert_eq!(bold_on_italic.lacking, Some(Lacking::Pairing));
        assert!(
            !bold_on_italic.tip.contains("no bold"),
            "the family has a bold: {}",
            bold_on_italic.tip
        );
        // The mirror, which is the commoner one: Italic on the bold.
        let italic_on_bold = emphasis(&mixed, "Foo", "Bold", Emphasis::Italic);
        assert_eq!(italic_on_bold.lacking, Some(Lacking::Pairing));
        assert!(
            !italic_on_bold.tip.contains("no italic"),
            "the family has an italic: {}",
            italic_on_bold.tip
        );

        // A family of one weight has neither, and says so.
        let alone = library(&[face("Zapfino", "Regular", 400, false)]);
        for which in [Emphasis::Bold, Emphasis::Italic] {
            assert_eq!(
                emphasis(&alone, "Zapfino", "Regular", which).lacking,
                Some(Lacking::Any),
                "{which:?}"
            );
        }

        // Nothing but a heavy display weight: the mark is lit and there is no way
        // back, which is a different sentence from "no bold".
        let heavy = library(&[face("Slab Only", "Black", 900, false)]);
        let mark = emphasis(&heavy, "Slab Only", "Black", Emphasis::Bold);
        assert!(mark.on, "the only face of the family is its own bold");
        assert_eq!(mark.lacking, Some(Lacking::WayBack));
        // An italic-only face is the same shape one slant along.
        let script = library(&[face("Slant Only", "Italic", 400, true)]);
        let mark = emphasis(&script, "Slant Only", "Italic", Emphasis::Italic);
        assert!(mark.on);
        assert_eq!(mark.lacking, Some(Lacking::WayBack));

        // **`WayBack` where the family does have lighter faces**, which is what
        // the sentence used to claim it did not. Every upright of this family is
        // on the bold side, so the lit Bold is refused; SemiBold is lighter than
        // Bold and is the row directly above it in the list.
        let heavy_pair = library(&[
            face("Two Bolds", "SemiBold", 600, false),
            face("Two Bolds", "Bold", 700, false),
        ]);
        let mark = emphasis(&heavy_pair, "Two Bolds", "Bold", Emphasis::Bold);
        assert!(mark.on);
        assert_eq!(mark.lacking, Some(Lacking::WayBack));
        assert!(
            !mark.tip.contains("nothing lighter"),
            "SemiBold is lighter and is in the list: {}",
            mark.tip
        );

        // The mirror, one slant along: an upright face exists, at another weight.
        let slant_pair = library(&[
            face("Crossed", "Italic", 400, true),
            face("Crossed", "Bold", 700, false),
        ]);
        let mark = emphasis(&slant_pair, "Crossed", "Italic", Emphasis::Italic);
        assert!(mark.on);
        assert_eq!(mark.lacking, Some(Lacking::WayBack));
        assert!(
            !mark.tip.contains("Every face") && !mark.tip.contains("every face"),
            "an upright face is in the list: {}",
            mark.tip
        );

        // And a family that is not here names the font rather than a weight.
        let missing = emphasis(&mixed, "Not Installed", "Regular", Emphasis::Bold);
        assert_eq!(missing.lacking, Some(Lacking::Family));
        assert!(
            missing.tip.contains("not on this machine"),
            "{}",
            missing.tip
        );
    }

    /// A family the machine does not have offers neither mark, rather than
    /// bolding whatever `resolve` substituted.
    ///
    /// The substitution note above the marks is what says the family is missing.
    /// A live Bold there would be a control acting on a typeface nobody chose,
    /// and it would write that typeface's style name into the panel's own field.
    #[test]
    fn a_family_that_is_not_here_offers_neither_mark() {
        let mut ed = Editor::default();
        ed.text.fonts.hold_at_builtin();
        ed.text.family = "A Foundry Face Nobody Has".to_string();
        for which in [Emphasis::Bold, Emphasis::Italic] {
            let toggle = emphasis(
                ed.text.fonts.library(),
                &ed.text.family,
                &ed.text.style,
                which,
            );
            assert_eq!(
                toggle.lacking,
                Some(Lacking::Family),
                "{which:?} was offered for a missing family, or refused for the wrong reason"
            );
            assert!(!toggle.on);
            assert!(!apply_emphasis(&mut ed, which));
        }
    }

    /// Every state one of these marks can be in has a finished sentence.
    ///
    /// All twelve, because `emphasis_tip`'s wildcard-free `match` is what forces
    /// one to exist and this is what checks it is a sentence rather than a code.
    ///
    /// **Both enumerations are guarded by an exhaustive match that indexes the
    /// array**, which is the rule CLAUDE.md states and which the first draft of
    /// this test broke twice over: iterating a hand-written list can only ever
    /// check what is in it. A variant added to either enum now fails to compile
    /// here, and a variant left out of `Emphasis::ALL` is an out-of-bounds panic
    /// rather than a quiet pass.
    #[test]
    fn every_state_a_style_mark_can_be_in_has_a_finished_sentence() {
        // `Emphasis::ALL` is hand-written, so the arms index it: a short array
        // panics where merely walking it would say nothing.
        for which in Emphasis::ALL {
            match which {
                Emphasis::Bold => assert_eq!(Emphasis::ALL[0], Emphasis::Bold),
                Emphasis::Italic => assert_eq!(Emphasis::ALL[1], Emphasis::Italic),
            }
        }
        for which in Emphasis::ALL {
            for lacking in [
                None,
                Some(Lacking::Family),
                Some(Lacking::Style),
                Some(Lacking::Any),
                Some(Lacking::Pairing),
                Some(Lacking::WayBack),
            ] {
                // Exhaustive over `Lacking` rather than trusting the array: a
                // variant added to it fails to compile here, which the array
                // alone would not.
                if let Some(r) = lacking {
                    match r {
                        Lacking::Family
                        | Lacking::Style
                        | Lacking::Any
                        | Lacking::Pairing
                        | Lacking::WayBack => {}
                    }
                }
                let tip = emphasis_tip(which, lacking);
                let at = format!("{which:?}, lacking {lacking:?}");
                assert!(tip.ends_with('.'), "{at} is not a sentence: {tip:?}");
                assert!(tip.len() > 20, "{at} is a code, not a sentence: {tip:?}");
                // No em-dash in anything the interface draws.
                assert!(!tip.contains('—'), "{at} carries an em-dash: {tip:?}");
                // British spelling, and the one word that would give it away.
                assert!(!tip.contains("italicize"), "{at}: {tip:?}");
                if lacking.is_some() {
                    // A refusal says what is missing rather than only that the
                    // control is dead, and it says where to go: a font to
                    // install, another font, or a row of this family's list.
                    assert!(
                        tip.contains("this machine") || tip.contains("family"),
                        "{at} does not say what is missing: {tip:?}"
                    );
                    // **And it must not claim more than `can_restyle` checked.**
                    // That predicate is about this slant on one side of the
                    // weight, never about the whole family, and two sentences
                    // said "nothing lighter" and "every face of this family" for
                    // families that plainly had one.
                    for overclaim in ["nothing lighter", "Every face", "every face"] {
                        assert!(
                            !tip.contains(overclaim),
                            "{at} claims more than was checked ({overclaim:?}): {tip:?}"
                        );
                    }
                }
            }
        }
        // The five refusals are five sentences, not one repeated. That is the
        // whole of what the enum bought over a boolean, and it was wrong before:
        // the same "this family has no bold" was shown for a family that had one,
        // for a font that was not installed, and for a style name that had gone
        // stale.
        for which in Emphasis::ALL {
            let mut said: Vec<&str> = [
                Lacking::Family,
                Lacking::Style,
                Lacking::Any,
                Lacking::Pairing,
                Lacking::WayBack,
            ]
            .iter()
            .map(|r| emphasis_tip(which, Some(*r)))
            .collect();
            said.sort_unstable();
            said.dedup();
            assert_eq!(said.len(), 5, "{which:?} repeats a refusal: {said:?}");
        }
    }

    /// **A style name that has gone stale is its own refusal**, and it says the
    /// family is here.
    ///
    /// The library is replaced wholesale when a scan lands and when the font
    /// folder preference changes, so a style chosen from the old list can stop
    /// existing while the family survives. `FontLibrary::exact` answers about the
    /// *pair*, so reading its `None` as a missing family put "that font is not on
    /// this machine, choose a font the list has" beside a picker naming a font
    /// that was, inches under `substitution_note` saying the opposite.
    ///
    /// And **both marks are refused**, where Bold used to be live: `restyle`
    /// finds the family's bold with no face in hand, so a press rewrote
    /// `TextState::style` to a real name and silently discarded the choice the
    /// note above promises is kept.
    #[test]
    fn a_stale_style_name_is_told_apart_from_a_missing_font() {
        let mut ed = Editor::default();
        ed.text.fonts.hold_at_builtin();
        ed.text.style = "Ultra Condensed Black Italic".to_string();
        let before = ed.text.style.clone();

        for which in Emphasis::ALL {
            let mark = emphasis(
                ed.text.fonts.library(),
                &ed.text.family,
                &ed.text.style,
                which,
            );
            assert_eq!(mark.lacking, Some(Lacking::Style), "{which:?}");
            assert!(
                !mark.on,
                "{which:?} read a state off a face that is not here"
            );
            assert!(
                !mark.tip.contains("not on this machine"),
                "{which:?} says the family is missing when it is here: {}",
                mark.tip
            );
            assert!(!apply_emphasis(&mut ed, which), "{which:?} moved the style");
        }
        assert_eq!(
            ed.text.style, before,
            "a mark rewrote the style the picker is keeping on purpose"
        );

        // The panel does say something about it, in the place that state belongs:
        // the substitution note, which names the style rather than the font.
        assert_eq!(
            ed.text
                .fonts
                .library()
                .substituted(&ed.text.family, &ed.text.style)
                .map(|(what, _)| what),
            Some(Substitution::Style)
        );
    }

    /// **Every style the picker offers can actually be set.**
    ///
    /// The style list is the family's own subfamily names and a variable font's
    /// named instances, so a row of it must resolve back to the face it came from
    /// and must rasterise.
    ///
    /// **This is the shipped font's half only, and it can only see the second of
    /// those.** Archivo has no duplicate style names, so the assertion that a
    /// listed name resolves to a face with that name is `exact`'s own predicate
    /// read back and proves nothing here; the case where two faces of one family
    /// share a name is `FontLibrary::insert`'s to refuse, and
    /// `a_style_name_identifies_one_face_of_its_family` is what checks it. What
    /// this one is worth is the *rasterisation*: every instance the picker offers
    /// is one `text::set` will take, which no amount of reasoning about names
    /// establishes.
    #[test]
    fn every_style_the_picker_offers_can_actually_be_set() {
        let mut ed = Editor::default();
        ed.text.fonts.hold_at_builtin();
        ed.text.block.text = "Hxg".to_string();
        ed.text.block.size = 24.0;

        let styles: Vec<String> = ed
            .text
            .fonts
            .library()
            .family(&ed.text.family)
            .iter()
            .map(|f| f.style.clone())
            .collect();
        assert!(styles.len() > 4, "only {styles:?}");

        for style in styles {
            ed.text.style = style.clone();
            let face = ed
                .text
                .fonts
                .library()
                .exact(&ed.text.family, &style)
                .expect("a listed style resolves")
                .clone();
            assert!(
                face.style.eq_ignore_ascii_case(&style),
                "{style} resolved to {}",
                face.style
            );
            assert!(
                ed.text.set().is_ok(),
                "{style} is in the list and will not set"
            );
        }
    }

    // --- editing a placed text layer ----------------------------------------

    /// A record for the layer of a default document, in the built-in face so it
    /// resolves on every machine.
    fn a_record(caption: &str) -> umber_core::textobj::TextObject {
        use umber_core::textobj::{Placement, TextFace, TextObject};
        let ed = Editor::default();
        let face = ed.text.face().expect("the built-in face").clone();
        TextObject::new(
            TextBlock {
                text: caption.to_string(),
                size: 48.0,
                line_spacing: 1.2,
                tracking: 0.0,
                align: Align::Left,
            },
            TextFace {
                family: face.family,
                style: face.style,
                postscript: String::new(),
            },
            umber_core::Color::from_srgb_u8(30, 30, 30, 255),
            Placement::identity(umber_core::PixelRect {
                x: 10,
                y: 20,
                width: 100,
                height: 40,
            }),
        )
    }

    /// **Selecting a text layer shows that layer's record, and the block being
    /// composed comes back afterwards.**
    ///
    /// A block being composed belongs to the person and a record belongs to the
    /// picture — the division that keeps `TextState` above the
    /// `--- documents ---` line. Clicking a text layer to fix a typo and losing
    /// the caption you were half way through typing is what the stash prevents.
    #[test]
    fn selecting_a_text_layer_shows_its_record_and_gives_the_composed_block_back() {
        let mut ed = Editor::default();
        ed.text.fonts.hold_at_builtin();
        ed.text.block.text = "Half a caption".to_string();
        ed.text.block.size = 31.0;

        // Nothing selected is a text layer, so nothing changes.
        sync_editing(&mut ed);
        assert!(ed.text.editing.is_none());
        assert_eq!(ed.text.block.text, "Half a caption");

        assert!(ed.layers.set_text(0, a_record("On the canvas")));
        sync_editing(&mut ed);
        let editing = ed.text.editing.as_ref().expect("the layer is text");
        assert_eq!(editing.slot, ed.layers.active_slot().expect("a slot"));
        assert_eq!(editing.doc, ed.session.active_id());
        assert_eq!(
            ed.text.block.text, "On the canvas",
            "the panel is still showing the composed block"
        );
        assert_eq!(ed.text.block.size, 48.0);
        // The colour is the record's, not the palette's — otherwise fixing a
        // typo would repaint the caption in whatever happened to be in hand.
        assert_eq!(ed.text_colour(), a_record("x").colour);
        assert_ne!(ed.text_colour(), ed.color);

        // Off it again, and what was being composed is back.
        assert!(ed.layers.take_text(0).is_some());
        sync_editing(&mut ed);
        assert!(ed.text.editing.is_none());
        assert_eq!(ed.text.block.text, "Half a caption");
        assert_eq!(ed.text.block.size, 31.0);
        assert_eq!(ed.text_colour(), ed.color);
    }

    /// **The panel cannot show one layer's record while editing another's**,
    /// which is what keying on the slot rather than the row buys.
    ///
    /// Stack order is a `Vec` order, so a reorder moves every row and moves no
    /// slot. A panel keyed on the selected *position* would go on believing it
    /// was editing the layer it picked up while Update wrote to whichever layer
    /// had moved into that row.
    #[test]
    fn the_panel_follows_the_layer_it_is_editing_rather_than_the_row() {
        let mut ed = Editor::default();
        ed.text.fonts.hold_at_builtin();
        let bottom = ed.layers.active_slot().expect("a slot");
        assert!(ed.layers.set_text(0, a_record("Bottom")));
        sync_editing(&mut ed);
        assert_eq!(ed.text.editing.as_ref().map(|e| e.slot), Some(bottom));

        // A second text layer, selected: the panel switches to it whole.
        let top = ed.layers.add().expect("room");
        assert_ne!(top, bottom);
        assert!(ed.layers.set_text(1, a_record("Top")));
        ed.layers.set_active(1);
        sync_editing(&mut ed);
        assert_eq!(ed.text.editing.as_ref().map(|e| e.slot), Some(top));
        assert_eq!(ed.text.block.text, "Top");

        // Reordering moves the rows and moves no slot, so the panel is still
        // editing the same layer and still showing its record.
        assert!(ed.layers.move_down(1).is_some());
        sync_editing(&mut ed);
        assert_eq!(
            ed.text.editing.as_ref().map(|e| e.slot),
            Some(top),
            "the panel followed the row instead of the layer"
        );
        assert_eq!(ed.text.block.text, "Top");
        assert_eq!(ed.layers.active_slot(), Some(top));
    }

    /// **A record that changes under the panel is picked up again**, which an
    /// undo is the ordinary way to do.
    ///
    /// Stepping back over a text edit puts the older record on the layer. The
    /// panel would otherwise go on showing the newer caption over a canvas that
    /// no longer has it, with Update reading "nothing has changed" and therefore
    /// disabled: the two irreconcilable, and the panel the one telling the lie.
    ///
    /// **And the block being composed still comes back afterwards**, which is
    /// the half a reload written carelessly loses: the stash is restored and
    /// re-taken, rather than overwritten with the record it is being swapped
    /// for.
    #[test]
    fn a_record_that_changed_under_the_panel_is_read_again() {
        let mut ed = Editor::default();
        ed.text.fonts.hold_at_builtin();
        ed.text.block.text = "Half a caption".to_string();

        assert!(ed.layers.set_text(0, a_record("Set again")));
        sync_editing(&mut ed);
        assert_eq!(ed.text.block.text, "Set again");

        // What an undo does: the older record goes back on the layer, with
        // nothing about the panel changed.
        assert!(ed.layers.take_text(0).is_some());
        assert!(ed.layers.set_text(0, a_record("As it was")));
        sync_editing(&mut ed);
        assert_eq!(
            ed.text.block.text, "As it was",
            "the panel kept showing a caption the layer no longer has"
        );
        assert_eq!(
            ed.text
                .editing
                .as_ref()
                .map(|e| e.original.block.text.clone()),
            Some("As it was".to_string()),
            "Update would compare against the record that was undone"
        );

        // Typing does **not** reload it: the comparison is against the record
        // the panel picked up, never against what the controls hold, or every
        // keystroke would be thrown away.
        ed.text.block.text.push('!');
        sync_editing(&mut ed);
        assert_eq!(ed.text.block.text, "As it was!");

        // And the composing block is still there to come back to.
        assert!(ed.layers.take_text(0).is_some());
        sync_editing(&mut ed);
        assert!(ed.text.editing.is_none());
        assert_eq!(
            ed.text.block.text, "Half a caption",
            "the reload swallowed the stashed composing block"
        );
    }

    /// Update is offered exactly when setting the text again would do something
    /// it can do.
    ///
    /// Every cell, because two of them are the ones that damage something. A
    /// **locked** layer must not be written at all — this is the sixth operation
    /// that writes pixels and it needs the gate every other one has — and a font
    /// that is not here must never be substituted, since `TextFace::resolve` is
    /// exact precisely so that a caption is not redrawn in a face its author did
    /// not choose.
    #[test]
    fn update_is_refused_for_every_reason_it_should_be() {
        let too_large = TextError::TooLarge {
            width: 9000,
            height: 9000,
        };
        let mut said: Vec<String> = Vec::new();
        for (locked, face_here, empty, refused, unchanged, why) in [
            (true, true, false, None, false, "a locked layer"),
            (
                false,
                false,
                false,
                None,
                false,
                "the boxes name a font that is not here",
            ),
            (false, true, true, None, false, "nothing typed"),
            (false, true, false, Some(too_large), false, "past the cap"),
            (false, true, false, None, true, "nothing changed"),
            (
                true,
                false,
                true,
                Some(too_large),
                true,
                "everything at once",
            ),
        ] {
            let state = update_state(locked, face_here, empty, refused, unchanged);
            assert!(!state.enabled, "Update was live for {why}");
            assert!(state.tooltip.ends_with('.'), "{why}: {}", state.tooltip);
            assert!(!state.tooltip.contains('—'), "{why}: {}", state.tooltip);
            said.push(state.tooltip.to_string());
        }
        let state = update_state(false, true, false, None, false);
        assert!(state.enabled, "Update was refused with nothing wrong");
        assert!(!state.tooltip.contains('—'), "{}", state.tooltip);
        // It says what moving the caption costs rather than implying the
        // transform tool is beside it doing the other half: picking it up makes
        // the layer paint.
        assert!(
            state.tooltip.contains("makes the layer paint"),
            "{}",
            state.tooltip
        );

        // A lock outranks everything: the artist is not helped by also being
        // told about the size, and the layer is the thing to fix first. The
        // same order `place_state` keeps.
        assert_eq!(
            update_state(true, false, true, Some(too_large), true).tooltip,
            update_state(true, true, false, None, false).tooltip
        );
        assert!(said[0].contains("locked"), "{}", said[0]);

        // **The font refusal names the boxes and not the record**, because that
        // is the pair `update_text_layer` resolves. Reading the record's face
        // instead left the family and style dropdowns live and doing nothing on
        // a layer whose font had gone, which is the one way out for somebody
        // who has the document and not the font.
        assert!(said[1].contains("boxes above"), "{}", said[1]);
        assert!(said[1].contains("substitute"), "{}", said[1]);

        // Six refusals, six sentences.
        let mut unique = said.clone();
        unique.sort();
        unique.dedup();
        assert!(unique.len() >= 4, "refusals repeat each other: {said:?}");
    }

    /// **A record naming a font this machine has not got freezes the layer**,
    /// and the panel says which font before anything is pressed.
    ///
    /// `TextFace::resolve` is exact and never substitutes — `FontLibrary`'s own
    /// `resolve` is total and is the wrong one here, because re-rendering
    /// somebody's caption in a face its author did not choose changes the
    /// picture silently.
    ///
    /// **Frozen is a state and not a sentence**, so both halves are checked:
    /// Update is refused while the boxes still name the font that has gone, and
    /// it comes back the moment the artist picks one this machine has. That is
    /// the one way out for somebody who has the document and not the font, and
    /// it is not a substitution: a face chosen off the list is chosen.
    #[test]
    fn a_text_layer_whose_font_is_gone_is_frozen_and_names_it() {
        let mut ed = Editor::default();
        ed.text.fonts.hold_at_builtin();
        let mut record = a_record("A caption");
        record.face.family = "A Foundry Face Nobody Has".to_string();
        record.face.postscript = "AFontNobodyHas-Regular".to_string();
        assert!(ed.layers.set_text(0, record.clone()));
        sync_editing(&mut ed);

        let editing = ed.text.editing.as_ref().expect("editing");
        assert!(
            editing
                .original
                .face
                .resolve(ed.text.fonts.library())
                .is_none(),
            "the face resolved, so this case is not the one being tested"
        );
        // And `FontLibrary::resolve` would have answered with something, which
        // is exactly the difference.
        assert!(
            ed.text
                .fonts
                .library()
                .resolve(&record.face.family, &record.face.style)
                .is_some()
        );
        // The panel loaded the record's family into the boxes, so the pair the
        // pickers name is the pair that is missing, and Update is refused.
        assert_eq!(ed.text.family, "A Foundry Face Nobody Has");
        let names_a_real_face = |ed: &Editor| {
            umber_core::textobj::TextFace {
                family: ed.text.family.clone(),
                style: ed.text.style.clone(),
                postscript: String::new(),
            }
            .resolve(ed.text.fonts.library())
            .is_some()
        };
        assert!(!names_a_real_face(&ed));
        assert!(!update_state(false, names_a_real_face(&ed), false, None, false).enabled);

        // **And it thaws on a font the artist chooses.** Not a substitution:
        // this is a face picked off the list, which is the only way back for
        // somebody who has the document and not the font. Reading the record's
        // face instead left both dropdowns live and doing nothing.
        let here = ed
            .text
            .fonts
            .library()
            .faces()
            .first()
            .expect("the built-in face")
            .clone();
        ed.text.family = here.family.clone();
        ed.text.style = here.style.clone();
        assert!(names_a_real_face(&ed));
        assert!(update_state(false, names_a_real_face(&ed), false, None, false).enabled);
        // A lock still refuses it, because that is a different question.
        assert!(!update_state(true, names_a_real_face(&ed), false, None, false).enabled);

        // The panel draws the notice, which is what makes the frozen state
        // something an artist can act on rather than a dead button.
        let drawn = panel_text(|ed| {
            let mut record = a_record("A caption");
            record.face.family = "A Foundry Face Nobody Has".to_string();
            record.face.postscript = "AFontNobodyHas-Regular".to_string();
            assert!(ed.layers.set_text(0, record));
        });
        assert!(
            drawn.contains("A Foundry Face Nobody Has"),
            "the missing font was not named: {drawn}"
        );
        assert!(
            drawn.contains("Install the font"),
            "the notice does not say what would fix it: {drawn}"
        );
    }

    /// **The panel draws Update and Convert to paint for a text layer, and
    /// Place for anything else** — one control in that place, never a dead
    /// Place beside a live Update.
    ///
    /// Read off the shapes the real body emitted, because a test of
    /// `update_state` cannot see whether the row is drawn at all. That is the
    /// failure a critic found in this module before: the reading was right and
    /// the call site had been reverted.
    #[test]
    fn a_text_layer_gets_update_and_convert_where_an_ordinary_one_gets_place() {
        let ordinary = panel_text(|ed| ed.text.block.text = "Umber".to_string());
        assert!(ordinary.contains("Place"), "{ordinary}");
        assert!(!ordinary.contains("Update text"), "{ordinary}");
        assert!(!ordinary.contains("Convert to paint"), "{ordinary}");

        let text_layer = panel_text(|ed| {
            assert!(ed.layers.set_text(0, a_record("On the canvas")));
        });
        assert!(
            text_layer.contains("Update text"),
            "no way to set a text layer again: {text_layer}"
        );
        assert!(
            text_layer.contains("Convert to paint"),
            "no way out of being a text layer: {text_layer}"
        );
        assert!(
            !text_layer.contains("Place"),
            "a dead Place was drawn beside a live Update: {text_layer}"
        );
        // And it really is showing the layer's own caption.
        assert!(
            text_layer.contains("On the canvas"),
            "the panel drew a blank slate over a text layer: {text_layer}"
        );
    }

    /// The Text module at the panel's real width, in the states it can be in.
    ///
    /// Written rather than asserted for the reason `layers_panel_preview` is:
    /// what goes wrong in a panel body is a *layout*, and no assertion about
    /// widgets catches two controls drawn over each other at `metrics::PANEL`'s
    /// real 264 points. This one has a preview image, three rails, two
    /// dropdowns and a segmented picker to fit into that width, which is
    /// exactly the shape that fits in the abstract and does not on screen.
    ///
    /// ```sh
    /// cargo test -p umber-app text_panel_preview -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "writes preview PNGs and wants a GPU; run deliberately"]
    #[cfg(debug_assertions)]
    fn text_panel_preview() {
        use crate::dock::{Layout, PanelKind};
        use crate::docshot;
        use crate::theme::metrics;
        use egui::{Pos2, Rect, vec2};

        let Some(mut stage) = docshot::Stage::new() else {
            eprintln!("no GPU adapter: nothing to draw into. Skipped.");
            return;
        };
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/text-panel");
        std::fs::create_dir_all(&dir).expect("create the preview directory");

        // The last three are the ones worth looking at, and all three are a
        // *notice* landing in a column that already holds a preview image,
        // three rails, two dropdowns and a segmented picker — which is the
        // shape that fits in the abstract and overruns at `metrics::PANEL`'s
        // real 264 points.
        //
        // The fourth has two CJK ideographs Archivo has no glyph for, so the
        // notice naming them sits beside the preview that does not show them.
        // The fifth is past `text::MAX_PIXELS`: the small preview rasterises
        // perfectly well at `PREVIEW_EM`, so there is a picture, and the
        // refusal has to be readable under it with Place disabled. The sixth
        // names a family this machine does not have, which is the one notice
        // that is drawn between the pickers rather than under the preview.
        for (name, text, align, size, family) in [
            ("1-empty", "", Align::Left, 72.0, None),
            ("2-a-caption", "Umber", Align::Left, 72.0, None),
            (
                "3-several-lines",
                "Painted in Umber\non a Tuesday\nafternoon",
                Align::Centre,
                72.0,
                None,
            ),
            (
                "4-a-face-cannot-show-it",
                "Umber \u{5b57}\u{4f53}",
                Align::Left,
                72.0,
                None,
            ),
            (
                "5-past-the-cap",
                "A caption nobody could fit on a canvas",
                Align::Left,
                text::MAX_SIZE,
                None,
            ),
            (
                "6-a-substituted-face",
                "Umber",
                Align::Left,
                72.0,
                Some("A Foundry Face Nobody Has"),
            ),
        ] {
            let mut ed = Editor::default();
            ed.layout = Layout::default();
            // No scan, for `hold_at_builtin`'s own reason: a committed shot
            // whose face count is the number of fonts on a contributor's
            // machine is a picture of that machine, and case 6 would find its
            // "missing" family on a machine that happens to have it.
            ed.text.fonts.hold_at_builtin();
            ed.text.block.text = text.to_string();
            ed.text.block.align = align;
            ed.text.block.size = size;
            if let Some(family) = family {
                ed.text.family = family.to_string();
            }
            let palette = crate::theme::Palette::with_accent(ed.ui.theme, ed.ui.accent);
            let field = vec2(metrics::PANEL, 520.0);
            let rect = Rect::from_min_size(Pos2::ZERO, field);
            let image = stage.shoot(field, 2.0, &palette, palette.dock, |root| {
                let mut actions = UiActions::default();
                crate::panels::panel(root, &palette, &mut ed, &mut actions, PanelKind::Text, rect);
            });
            docshot::write_png(&dir.join(format!("{name}.png")), &image).expect("write the png");
        }
        println!("wrote 6 shots to {}", dir.display());
    }
}
