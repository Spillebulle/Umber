//! The Palette module: the colours in front of the artist, and the library of
//! palettes they are kept in.
//!
//! `umber_core::palette` is the model — what a palette is, what a library is,
//! how one is named, and the `.gpl` both directions go through. Nothing in that
//! module draws, and nothing in this one decides. Same division `dock.rs` keeps
//! against `panels.rs`.
//!
//! The shape is the brush library's, because the two answer the same question
//! and the arguments carry over one for one:
//!
//! - **The library is read once and kept in egui's memory**, not on `Editor`,
//!   for the reason [`crate::brushlib`]'s is: it is not per-document, it is a
//!   handful of kilobytes, and putting it on the editor would mean threading it
//!   through everything that builds one.
//! - **The palette in front is held by *id*, never by index.** Deleting a
//!   palette moves every index after it, and a stale index does not fail — it
//!   quietly selects a different palette. Exactly why `Editor::active_preset`
//!   is re-found by id after every rebuild.
//! - **The panel is a shortlist and the modal is the library.** The panel shows
//!   one palette and the colours in it; making, renaming, importing, exporting
//!   and deleting are the modal's, where there is room to say what each one
//!   does.
//! - **The modal is drawn from `panels::sidebars`, not from the panel body.**
//!   The layout is free to hide the Palette panel, and a modal that went with
//!   its panel could not be shut and could not be reopened.
//!
//! # What a click does
//!
//! A swatch takes its colour, through [`Editor::set_color`] — which is the same
//! door the eyedropper and the colour wells use, so the picker's hue survives a
//! grey being taken. The two marks inside a swatch's corners — remove on the
//! right, name on the left — are *allocated* every frame and *painted* only
//! while `Response::contains_pointer` is true of the swatch, which is the rule
//! a revealed control has to follow here: egui stops its hover search at the
//! topmost interactive widget, so a mark that only existed while its parent
//! reported `hovered` would blink out the moment the pointer reached it and
//! back in the frame after. `contains_pointer` is geometry — layer-aware and
//! clip-aware, unlike the raw pointer position — and geometry does not
//! oscillate.
//!
//! # Arranging, naming, and keeping a harmony
//!
//! A palette a painter can fill but not arrange holds its colours in the order
//! they were clicked, for ever; a `Swatch::name` the `.gpl` round trip carries
//! and the interface cannot author is a field only somebody else's file ever
//! fills in. Three rules cover the three controls that fix that.
//!
//! - **The drag is a model, [`crate::swatchdrag`], and a write happens at the
//!   drop and nowhere else.** Every [`PaletteLibrary`] write reaches the disk
//!   immediately — that is the whole shape of a directory of `.gpl` files — so
//!   a drag that saved as it aimed would be a file write per mouse move. What
//!   moves during the gesture is a mark; what changes the palette is the
//!   release, through [`edit_current`] like every other edit.
//! - **A colour is named in the panel, not in the library modal**, and that is
//!   a decision rather than a lapse from "the panel is a shortlist and the
//!   modal is the library". The modal is the library *of palettes*: its rows
//!   are palettes and it draws each one's colours as a fourteen-pixel band
//!   nobody can point at. A swatch belongs to the palette in front, and the
//!   panel is the only place there is one; putting the field in the modal
//!   would mean the modal drawing a second grid. It sits under the grid, which
//!   is the last thing in the panel body, so nothing above it moves when it
//!   opens — the rule `ticking_a_layer_does_not_move_the_layer_list` states for
//!   the layer list. It needs no `shortcuts::set_capturing`: `ui::draw` calls
//!   `shortcuts::set_typing(ctx.text_edit_focused())` once for the whole
//!   interface and a real `TextEdit` is covered by it.
//! - **A harmony goes in whole or not at all**, and its control is in this
//!   panel's header rather than beside the wheel that shows it. `colorpicker`
//!   draws pickers and knows nothing about a library, and a picker that wrote
//!   to one would be the layering this module's own division already refuses.
//!   The tooltip names the relation and how many colours it is, so the mark
//!   says what it will do without the Colour panel having to be on screen.

use std::path::Path;
use std::sync::Arc;

use egui::{
    Color32, CursorIcon, Frame, Id, Layout, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};

use umber_core::palette::{
    self, GPL_EXTENSION, Palette as ColourPalette, PaletteError, PaletteLibrary,
};
use umber_core::palimport;
use umber_core::{Hsv, Swatch};

use crate::controls;
use crate::editor::Editor;
use crate::icons::Icon;
use crate::swatchdrag;
use crate::tabs::Notice;
use crate::theme::{Palette, metrics, text};
use crate::ui::icon_button;
use crate::widgets::{self, DropdownWidth};

/// The library, or why there is none.
///
/// Two states rather than an `Option`, because "there is nowhere to keep
/// palettes on this system" is a sentence the controls have to be able to show
/// instead of simply being dead — the same reason `brushlib::Store` has a
/// `Broken` arm carrying the library's own wording.
#[derive(Clone)]
enum Store {
    Ready(Arc<PaletteLibrary>),
    Broken(String),
}

/// A rename in progress: which palette, what has been typed, and whether the
/// field still has to take the keyboard.
///
/// `focus` is consumed on the first frame. Asking for focus every frame instead
/// would take it straight back off anything else the user clicked — the same
/// arrangement `brushlib::Field` keeps.
#[derive(Clone)]
struct Renaming {
    id: String,
    text: String,
    focus: bool,
}

/// A colour being given a name: which palette, which position in it, the
/// colour that was sitting there when the field opened, and what has been
/// typed.
///
/// `rgb` is a structural guard rather than decoration, and it is the second of
/// two. The index is a position in one palette, the field outlives the frame it
/// was opened on, and both a remove and a drag can rearrange the grid under
/// it — so an index alone would eventually write the typed name onto a colour
/// nobody was naming, silently, straight into the file. Every other edit closes
/// the field, which is the first guard; this is what catches the one that gets
/// forgotten.
///
/// `focus` is consumed on the first frame, exactly as [`Renaming`]'s is.
#[derive(Clone)]
struct Naming {
    palette: String,
    index: usize,
    rgb: [u8; 3],
    text: String,
    focus: bool,
}

/// What a palette made out of pasted text is called before anybody renames it.
const PASTED_NAME: &str = "Pasted colours";

/// How a refusal about the pasted text names it. A phrase and not a path,
/// because there is no file and a made-up filename in an error message is one
/// somebody would go looking for.
const PASTE_SOURCE: &str = "The pasted text";

/// The most text the paste field will hold on to.
///
/// **Much smaller than `palimport::MAX_FILE_BYTES`, and for a different
/// reason.** That bound is on the *parse*, which is linear and cheap; this one
/// is on `egui::TextEdit::multiline`, which lays out the whole string and does
/// not virtualise, so a multi-megabyte paste is unbounded layout work on every
/// frame the galley cache misses — before the parse ever gets to say "too
/// large". A palette of `MAX_SWATCHES` colours is about eighty kilobytes of
/// text, so a quarter of a megabyte is generous for anything that is actually a
/// palette and is a size egui handles without noticing.
const MAX_PASTE_BYTES: usize = 256 * 1024;

/// How many lines of the pasted text are on screen at once.
///
/// A palette is a handful of colours, so five lines shows a whole Coolors link
/// or the top of a block of CSS and leaves the list of palettes room to be the
/// thing this modal is mostly for.
const PASTE_ROWS: usize = 5;

/// Roughly what the paste pane costs, taken off the list's own height so the
/// modal does not grow past the window when it opens.
const PASTE_BLOCK: f32 = 150.0;

/// Text somebody pasted, and what Umber found in it.
///
/// The colours are held rather than re-derived at the click, so the readout and
/// what the button will do cannot disagree — and so that the parse runs once
/// per edit rather than once per frame. `MAX_FILE_BYTES` of text through the
/// scanner sixty times a second is the thing that would make.
#[derive(Clone, Default)]
struct Pasting {
    text: String,
    found: Vec<Swatch>,
    losses: palimport::Losses,
    /// Why the text was refused whole, if it was. Separate from "nothing was
    /// found", which is not an error: an empty field is the ordinary state of a
    /// field nobody has typed in yet.
    refusal: Option<String>,
    /// Consumed on the first frame, exactly as [`Renaming`]'s is.
    focus: bool,
}

impl Pasting {
    /// Read the text again. Called from the one place the text can change.
    ///
    /// It **cuts** the text at [`MAX_PASTE_BYTES`] rather than only refusing
    /// it, and says so. Refusing alone would leave the field holding megabytes
    /// that `TextEdit` re-lays out for as long as the pane is open; cutting
    /// bounds every frame after the one that accepted the paste, which is the
    /// most that can be done from this side of egui. The cut is at a character
    /// boundary, or the `String` would not be one.
    fn reread(&mut self) {
        if self.text.len() > MAX_PASTE_BYTES {
            let cut = (0..=MAX_PASTE_BYTES)
                .rev()
                .find(|at| self.text.is_char_boundary(*at))
                .unwrap_or(0);
            self.text.truncate(cut);
            self.found.clear();
            self.losses = palimport::Losses::default();
            self.refusal = Some(format!(
                "That is more text than a palette can be, so it was cut at {} \
                 kB. Paste a list of colours rather than a whole file.",
                MAX_PASTE_BYTES / 1024
            ));
            return;
        }
        match palimport::text::parse(&self.text, PASTE_SOURCE) {
            Ok((found, losses)) => {
                self.found = found;
                self.losses = losses;
                self.refusal = None;
            }
            Err(e) => {
                self.found.clear();
                self.losses = palimport::Losses::default();
                self.refusal = Some(e.to_string());
            }
        }
    }
}

#[derive(Clone)]
struct State {
    store: Store,
    /// The palette in front, by id. See the module docs.
    selected: Option<String>,
    library_open: bool,
    renaming: Option<Renaming>,
    /// The colour whose name is being typed, if any.
    naming: Option<Naming>,
    /// The paste pane, while it is open.
    pasting: Option<Pasting>,
    /// The id of the palette whose Delete has been pressed once. Deleting a
    /// palette cannot be undone — the history covers painting only — so it
    /// asks.
    confirming: Option<String>,
}

impl State {
    fn library(&self) -> Option<&Arc<PaletteLibrary>> {
        match &self.store {
            Store::Ready(library) => Some(library),
            Store::Broken(_) => None,
        }
    }

    fn writable(&self) -> bool {
        matches!(self.store, Store::Ready(_))
    }

    /// The tooltip for a control that writes when there is nothing to write to.
    /// Never invented wording: it is what the library itself reported.
    fn why_not(&self) -> &str {
        match &self.store {
            Store::Ready(_) => "",
            Store::Broken(why) => why,
        }
    }

    /// The palette in front, if the id still names one.
    fn current(&self) -> Option<&ColourPalette> {
        let library = self.library()?;
        library.get(self.selected.as_deref()?)
    }

    /// Point at the first palette when nothing is selected, or when what was
    /// selected has gone.
    ///
    /// A library with one palette in it and nothing selected is a panel that
    /// looks broken, and after a delete somebody has to be shown *something*.
    fn settle_selection(&mut self) {
        let Some(library) = self.library() else {
            self.selected = None;
            return;
        };
        if self
            .selected
            .as_deref()
            .is_some_and(|id| library.get(id).is_some())
        {
            return;
        }
        self.selected = library.palettes().first().map(|p| p.id.clone());
    }
}

fn state_id() -> Id {
    Id::new("palette-library")
}

/// Read the state back, reading the library off disk on the first frame.
fn load(ctx: &egui::Context, ed: &mut Editor) -> State {
    if let Some(state) = ctx.data(|d| d.get_temp::<State>(state_id())) {
        return state;
    }

    let store = match PaletteLibrary::load() {
        Ok(library) => {
            // A file that would not read means a palette the artist made is not
            // in the list, which is worth one dialog on the first frame the
            // module is drawn. It uses the editor's own notice rather than a
            // strip of this module's, so there is one way a message reaches the
            // user rather than two that have to look alike.
            if !library.warnings().is_empty() {
                ed.notice = Some(Notice {
                    title: "Some palettes could not be read".to_owned(),
                    lines: library.warnings().to_vec(),
                });
            }
            Store::Ready(Arc::new(library))
        }
        Err(e) => Store::Broken(e.to_string()),
    };
    let mut state = State {
        store,
        selected: None,
        library_open: false,
        renaming: None,
        naming: None,
        pasting: None,
        confirming: None,
    };
    state.settle_selection();
    state
}

fn store(ctx: &egui::Context, state: State) {
    ctx.data_mut(|d| d.insert_temp(state_id(), state));
}

/// Run a write against the library and settle what it changed.
///
/// Every [`PaletteLibrary`] write reaches the disk immediately, so this is also
/// where a failed write becomes something the user can read rather than a log
/// line nobody sees. `None` means it did not happen — either there is no
/// library to write to, or the write failed and the notice already says so.
fn write<T>(
    state: &mut State,
    ed: &mut Editor,
    what: &str,
    op: impl FnOnce(&mut PaletteLibrary) -> Result<T, PaletteError>,
) -> Option<T> {
    let Store::Ready(library) = &mut state.store else {
        return None;
    };
    match op(Arc::make_mut(library)) {
        Ok(value) => {
            state.settle_selection();
            Some(value)
        }
        Err(e) => {
            ed.notice = Some(Notice {
                title: what.to_owned(),
                lines: vec![e.to_string()],
            });
            None
        }
    }
}

// ---------------------------------------------------------------------------
// The panel
// ---------------------------------------------------------------------------

/// The three marks in the Palette panel's header: save the colour in hand, save
/// its whole harmony, and open the library.
pub fn header_controls(ui: &mut Ui, p: &Palette, ed: &mut Editor) {
    let mut state = load(ui.ctx(), ed);

    // Right-to-left: added first lands furthest right, next to the close mark,
    // which is where the Brushes panel draws its own save.
    let room = state.current().is_some_and(ColourPalette::has_room);
    let can_add = state.writable() && room;
    let tip = if can_add {
        "Add the colour in hand to this palette".to_owned()
    } else {
        no_room_because(&state, 1)
    };
    if icon_button(ui, p, Icon::Plus, can_add, &tip) {
        add_current_colour(&mut state, ed);
    }

    // Beside the single Add, because it is a kind of Add. See the module docs
    // for why it is here rather than under the wheel that shows the harmony.
    let hues = ed.ui.harmony.hues(ed.hsv.h).len();
    let can_keep = state.writable() && state.current().is_some_and(|q| q.room_for(hues));
    let tip = if can_keep {
        format!(
            "{} harmony: add its {hues} colours to this palette",
            ed.ui.harmony.label()
        )
    } else {
        no_room_because(&state, hues)
    };
    if icon_button(ui, p, Icon::Harmony, can_keep, &tip) {
        keep_harmony(&mut state, ed);
    }

    if icon_button(
        ui,
        p,
        Icon::Grid,
        true,
        "Palettes: make, import and manage them",
    ) {
        state.library_open = true;
    }

    store(ui.ctx(), state);
}

/// Why a control that adds `count` colours is dead.
///
/// One function for both adding marks, because the three reasons are the same
/// three and the wording of them is what the disabled control has instead of an
/// action. Stated in this order deliberately: nowhere to write beats no palette
/// beats no room, since each later answer assumes the earlier one is not the
/// problem.
fn no_room_because(state: &State, count: usize) -> String {
    if !state.writable() {
        state.why_not().to_owned()
    } else if state.current().is_none() {
        "Make a palette first. The grid mark opens the library.".to_owned()
    } else if count == 1 {
        format!(
            "This palette already holds {} colours",
            palette::MAX_SWATCHES
        )
    } else {
        format!("There is not room for {count} more colours in this palette")
    }
}

/// The panel body: which palette, and the colours in it.
pub fn panel(ui: &mut Ui, p: &Palette, ed: &mut Editor) {
    let mut state = load(ui.ctx(), ed);

    if !state.writable() {
        forget_gesture(ui.ctx(), &mut state);
        controls::note(ui, p, state.why_not());
        store(ui.ctx(), state);
        return;
    }

    palette_picker(ui, p, &mut state);
    ui.add_space(8.0);

    if state.current().is_none() {
        forget_gesture(ui.ctx(), &mut state);
        empty_library(ui, p, ed, &mut state);
        store(ui.ctx(), state);
        return;
    }

    let act = swatch_grid(ui, p, ed, &state);

    // **The field is settled before the grid's click lands, and that ordering
    // is the whole of it.** A field losing focus and a mark being pressed are
    // one frame's worth of *different* events — clicking any mark on any swatch
    // while a name is being typed fires both — and `library_list` records what
    // happens when one slot has to carry both: the typed name is thrown away
    // with nothing to show for it. So the name is saved first and the mark
    // still does what it was pressed for, which is that rule applied one frame
    // earlier: `naming_field` has already cleared `naming` by the time the arms
    // below reach for it, so `Act::Name` opens on the next colour, `Act::Remove`
    // takes one away, and neither costs the name that was in the field.
    naming_field(ui, p, ed, &mut state);

    match act {
        Some(Act::Take(index)) => {
            if let Some(swatch) = state.current().and_then(|s| s.swatches.get(index)) {
                let colour = swatch.colour();
                ed.set_color(colour);
            }
        }
        Some(Act::Remove(index)) => {
            // Every position after this one is about to move, so an open field
            // would point at a colour that is no longer the one it was opened
            // on. Closed rather than re-aimed: the artist asked to remove a
            // colour, not to start naming its neighbour. In practice the click
            // that got here has already taken the keyboard off the field, so
            // the name is saved above and this clears an empty slot — which is
            // exactly why it is here, as the case that is left when it has not.
            state.naming = None;
            edit_current(&mut state, ed, "Could not save the palette", |palette| {
                palette.remove(index).is_some()
            });
        }
        Some(Act::Move { from, to }) => {
            state.naming = None;
            // The one write a drag makes, at the release. See the module docs:
            // a save reaches the disk immediately, so aiming must not write.
            edit_current(&mut state, ed, "Could not save the palette", |palette| {
                palette.move_swatch(from, to)
            });
        }
        Some(Act::Name(index)) => {
            state.naming = naming_for(&state, index);
        }
        None => {}
    }

    store(ui.ctx(), state);
}

/// Open the naming field on one colour of the palette in front.
///
/// `None` where the index names nothing, which is the same refusal
/// `Palette::name_swatch` makes and for the same reason — the index came from a
/// grid drawn against last frame's palette.
fn naming_for(state: &State, index: usize) -> Option<Naming> {
    let palette = state.current()?;
    Some(Naming {
        palette: palette.id.clone(),
        index,
        rgb: palette.swatches.get(index)?.rgb,
        text: palette.swatches[index].name.clone(),
        focus: true,
    })
}

/// Whether an open naming field still names the colour it was opened on.
///
/// A pure function of the state rather than a condition written inline where
/// the field is drawn, because [`Naming`] calls this its structural guard and a
/// guard nothing can check is a claim rather than a guard. Same division the
/// rest of this module keeps: the rule is testable without a window.
///
/// Three things have to hold — the palette in front is the one the field was
/// opened on, the index still names a colour, and that colour is still the one
/// that was there. The third is what catches a remove having shifted every
/// position after it.
fn naming_is_live(state: &State, naming: &Naming) -> bool {
    state.selected.as_deref() == Some(naming.palette.as_str())
        && state
            .current()
            .and_then(|palette| palette.swatches.get(naming.index))
            .is_some_and(|swatch| swatch.rgb == naming.rgb)
}

/// The colour chip beside the naming field: which colour is being named, said
/// by showing it rather than by a word.
const NAMING_CHIP: f32 = 16.0;

/// The field that gives one colour a name, under the grid.
///
/// See the module docs for why it is in the panel rather than in the library
/// modal, and why it needs no `shortcuts::set_capturing`.
fn naming_field(ui: &mut Ui, p: &Palette, ed: &mut Editor, state: &mut State) {
    // Nothing at all unless the field is open, on the palette in front, and
    // still over the colour it was opened on. See [`Naming`]: the alternative
    // is a name written onto a colour nobody was naming.
    let live = state
        .naming
        .as_ref()
        .is_some_and(|naming| naming_is_live(state, naming));
    if !live {
        state.naming = None;
        return;
    }
    let Some(naming) = state.naming.as_mut() else {
        return;
    };

    // What the field decided this frame: the position, and either the name to
    // set or nothing for an abandoned edit. Collected rather than applied
    // inside the layout, because applying it needs the state the field is
    // borrowing.
    let mut done: Option<(usize, Option<String>)> = None;
    let rgb = naming.rgb;
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let (chip, shown) = ui.allocate_exact_size(Vec2::splat(NAMING_CHIP), Sense::hover());
        ui.painter().rect_filled(
            chip,
            metrics::RADIUS,
            Color32::from_rgb(rgb[0], rgb[1], rgb[2]),
        );
        // An outline whatever the colour, or a chip the shade of the panel has
        // no edge — the rule the grid's own swatches follow.
        ui.painter().rect_stroke(
            chip,
            metrics::RADIUS,
            Stroke::new(1.0, p.border),
            StrokeKind::Inside,
        );
        // `Swatch::hex` and not a `format!` written here: there is one
        // statement of what a colour reads as, and the grid's own tooltip four
        // hundred lines up uses it.
        shown.on_hover_text(Swatch::new(rgb).hex());

        let field = ui.add(
            egui::TextEdit::singleline(&mut naming.text)
                .desired_width(ui.available_width())
                .hint_text("Name this colour")
                .font(egui::FontId::proportional(text::SMALL)),
        );
        if naming.focus {
            field.request_focus();
            naming.focus = false;
        }
        if field.lost_focus() {
            // Escape abandons; anything else — Enter, or a click elsewhere —
            // keeps what was typed, **including nothing**. Clearing the field is
            // how a name is taken off again, so this cannot borrow the palette
            // rename's shortcut of reading an empty field as Escape: there, an
            // empty name is one the model would substitute for, and here it is
            // the answer.
            let typed = naming.text.clone();
            done = Some((
                naming.index,
                (!ui.input(|i| i.key_pressed(egui::Key::Escape))).then_some(typed),
            ));
        }
    });

    if let Some((index, typed)) = done {
        state.naming = None;
        if let Some(name) = typed {
            edit_current(state, ed, "Could not save the palette", |palette| {
                palette.name_swatch(index, &name)
            });
        }
    }
}

/// Which palette is in front. A dropdown rather than a row of tabs: a library
/// can hold dozens, and there is one dropdown in this interface.
fn palette_picker(ui: &mut Ui, p: &Palette, state: &mut State) {
    let Some(library) = state.library().cloned() else {
        return;
    };
    let label = state
        .current()
        .map_or("No palettes", |palette| palette.name.as_str())
        .to_owned();
    let count = state.current().map(|palette| palette.len().to_string());

    let mut trigger = widgets::Dropdown::new(&label).width(DropdownWidth::Fill);
    if let Some(count) = &count {
        trigger = trigger.trailing(count);
    }
    let mut picked = None;
    widgets::dropdown(ui, p, trigger, |ui| {
        if library.is_empty() {
            ui.label(
                egui::RichText::new("Nothing here yet")
                    .size(text::TINY)
                    .color(p.text_dim),
            );
        }
        for palette in library.palettes() {
            if ui
                .selectable_label(
                    state.selected.as_deref() == Some(palette.id.as_str()),
                    &palette.name,
                )
                .clicked()
            {
                picked = Some(palette.id.clone());
            }
        }
    });
    if let Some(id) = picked {
        state.selected = Some(id);
    }
}

/// What a click in the grid asked for.
///
/// Collected and applied by the caller rather than acted on inside the loop,
/// because both arms need the palette mutably and the loop is holding it to
/// draw the rest of the row.
enum Act {
    Take(usize),
    Remove(usize),
    /// Open the naming field on this colour.
    Name(usize),
    /// A drag that has been let go: the colour at `from` lands at `to`.
    Move {
        from: usize,
        to: usize,
    },
}

/// How many colours fit across, given the room and what the palette asks for.
///
/// `columns` is `.gpl`'s own header — how whoever made the palette laid it out
/// — and it is honoured as a *maximum* rather than as the answer. A palette
/// authored in fours then reads as fours wherever there is room for four, and a
/// palette authored in sixteen does not force sixteen unreadable slivers into a
/// panel 264 px wide.
fn grid_columns(width: f32, wanted: u32) -> usize {
    let step = metrics::PALETTE_SWATCH + metrics::PALETTE_SWATCH_GAP;
    let fits = (((width + metrics::PALETTE_SWATCH_GAP) / step).floor() as usize).max(1);
    if wanted > 0 {
        fits.min(wanted as usize)
    } else {
        fits
    }
}

/// Where one swatch sits in a grid whose top-left corner is `origin`.
fn swatch_rect(origin: egui::Pos2, index: usize, columns: usize) -> Rect {
    let columns = columns.max(1);
    let step = metrics::PALETTE_SWATCH + metrics::PALETTE_SWATCH_GAP;
    Rect::from_min_size(
        pos2(
            origin.x + (index % columns) as f32 * step,
            origin.y + (index / columns) as f32 * step,
        ),
        egui::Vec2::splat(metrics::PALETTE_SWATCH),
    )
}

/// A mark tucked into one of a swatch's top corners.
const CORNER_MARK: f32 = 11.0;

/// Where the remove mark sits, given the swatch it belongs to: the top-right
/// corner.
///
/// Its own function so the containment can be checked without a `Ui`: a mark
/// that reached outside its swatch would sit over the neighbour and take its
/// clicks, so pointing at one colour would remove the one before it.
fn remove_rect(cell: Rect) -> Rect {
    Rect::from_min_size(
        pos2(cell.right() - CORNER_MARK - 1.0, cell.top() + 1.0),
        Vec2::splat(CORNER_MARK),
    )
}

/// Where the naming mark sits: the opposite corner, top-left.
///
/// Opposite rather than beside, because at `PALETTE_SWATCH`'s 26 px two 11 px
/// marks pushed together would leave two pixels of colour between them and no
/// way to tell which is which by position. Tested against [`remove_rect`] as a
/// pair — a mark overlapping the other would put "name this" and "remove this"
/// on the same pixels, and remove is the one that cannot be taken back.
fn name_rect(cell: Rect) -> Rect {
    Rect::from_min_size(
        pos2(cell.left() + 1.0, cell.top() + 1.0),
        Vec2::splat(CORNER_MARK),
    )
}

/// The rectangle the drop mark is drawn on, given the cell it names.
///
/// Its own function, and the *only* statement of it, because the test that says
/// the mark covers no colour and reaches no neighbour has to measure what
/// [`drop_ring`] actually draws. A test that recomputed this expression would
/// be checking its own arithmetic: widen it here to eat the neighbour and every
/// assertion would still pass. That is the failure CLAUDE.md records as a guard
/// covering a copy of the code rather than the code, and it is worth a function
/// to close.
///
/// Half the gap either side, less half the stroke, is exactly the room
/// `PALETTE_SWATCH_GAP` leaves between two colours.
fn drop_ring_rect(cell: Rect) -> Rect {
    cell.expand(metrics::PALETTE_SWATCH_GAP * 0.5 - 0.5)
}

/// Where a dragged colour would land: a dashed accent ring in the gap **around**
/// the cell it would take.
///
/// Two decisions about the *mark*, and each is against something the layer list
/// does.
///
/// **Around rather than over.** `panels::drop_slot` washes the row it names
/// with a tenth of the accent. A swatch cannot take one: the wash would tint
/// the colour, and a grid whose colours are not the colours they say is the one
/// thing this panel must never do. `PALETTE_SWATCH_GAP` is four pixels, so half
/// of it either side is exactly the room the ring needs and it never crosses
/// into a neighbour.
///
/// **Dashed, because the grid already owns the solid accent stroke.** The
/// swatch holding the colour in hand is drawn with a two-pixel accent outline
/// inside its own edge. A *solid* accent ring a pixel and a half outside that
/// would be the same mark saying a different thing, which is precisely the
/// failure `drop_slot` was written to record: borrowing the selected row's fill
/// made "this lands here" and "this is selected" indistinguishable.
///
/// **Square-cornered**, because at a pixel and a half's offset a rounded ring
/// traces the swatch's own outline and reads as a thicker copy of it.
///
/// **This should be `panels::dashed_rect` and is not, and the reason is not a
/// design one.** That function at a radius below half a pixel returns these
/// five points exactly — see `panels::rounded_outline`, which short-circuits to
/// the four corners — so `dashed_rect(painter, ring, 0.0, …)` draws the
/// identical mark. It is private to `panels`, and the change that would fix
/// this is one word on its signature. Until that is made, `metrics::DASH` and
/// `metrics::DASH_GAP` are at least the one statement of the rhythm, so the
/// dock's indicator, the layer list's slot and this cannot come to disagree
/// about what a dashed mark in this interface looks like.
fn drop_ring(painter: &egui::Painter, p: &Palette, cell: Rect) {
    let ring = drop_ring_rect(cell);
    let corners = [
        ring.left_top(),
        ring.right_top(),
        ring.right_bottom(),
        ring.left_bottom(),
        ring.left_top(),
    ];
    painter.extend(egui::Shape::dashed_line(
        &corners,
        Stroke::new(2.0, p.accent),
        metrics::DASH,
        metrics::DASH_GAP,
    ));
}

/// Which colour a press at `at` picks up, if any.
///
/// [`swatchdrag::cell_pressed`] with the two corner marks taken out, and that
/// subtraction is not tidiness. A mark sits *inside* its swatch, so the plain
/// containment test accepts a press on one — and egui calls a press a drag on
/// **time** alone: `is_decidedly_dragging` is true once `max_click_duration`
/// (six tenths of a second) has passed with the button held, whatever the
/// pointer did. So holding Remove while deciding, and letting go a cell over,
/// used to rearrange the palette instead of removing a colour — silently, with
/// the `.gpl` written on the spot and no undo for a palette anywhere in Umber.
///
/// The layer list has the same shape with the eye toggle inside its row and
/// gets away with it, because a reorder there records an `EditKind::MoveLayer`
/// and this records nothing.
///
/// A pure function of the cells and a point, so the rule is checkable without a
/// window — which is the only way anything about a drag begun by *waiting* can
/// be checked at all.
fn drag_origin(cells: &[swatchdrag::Cell], at: egui::Pos2) -> Option<usize> {
    let index = swatchdrag::cell_pressed(cells, at)?;
    let cell = cells.iter().find(|cell| cell.index == index)?;
    let on_a_mark = remove_rect(cell.rect).contains(at) || name_rect(cell.rect).contains(at);
    (!on_a_mark).then_some(index)
}

/// Where the colour being carried is kept between frames.
///
/// In egui's temporary store rather than on `Editor`, for the reason the layer
/// drag's is: it belongs to this grid, not to a document.
fn drag_id() -> Id {
    Id::new("palette-swatch-drag")
}

/// Abandon whatever gesture the grid was in the middle of.
///
/// Called wherever the grid stops being drawn — the module removed from the
/// layout, the library gone, an empty palette selected — and both halves are
/// needed for the same reason. A `Drag` in the store outlives the grid that
/// made it: it can never *act*, because [`swatchdrag::Drag::palette`] guards
/// that, but it is an entry that would sit in egui's memory for the rest of the
/// session. An open naming field is worse than untidy: the widget is not drawn
/// while the module is closed, so egui drops its focus without the field ever
/// reporting `lost_focus`, and reopening the module brings back a field that is
/// unfocused, cannot report a focus it never lost, and can therefore never
/// commit what is typed into it.
fn forget_gesture(ctx: &egui::Context, state: &mut State) {
    state.naming = None;
    ctx.data_mut(|d| d.remove::<swatchdrag::Drag>(drag_id()));
}

fn swatch_grid(ui: &mut Ui, p: &Palette, ed: &Editor, state: &State) -> Option<Act> {
    let library = state.library()?.clone();
    let id = state.selected.as_deref()?;
    let palette = library.get(id)?;

    if palette.is_empty() {
        controls::note(
            ui,
            p,
            "Nothing in this palette yet. The plus above adds the colour you \
             are painting with.",
        );
        return None;
    }

    // What is being carried, if anything. A drag naming another palette is not
    // this grid's: the palette in front can change while one sits in the store,
    // and an index means nothing without the palette it indexes. Dropped rather
    // than carried across, or a release would rearrange a palette nobody was
    // dragging.
    let mut drag: Option<swatchdrag::Drag> = ui.ctx().data(|d| d.get_temp(drag_id()));
    if drag.as_ref().is_some_and(|carried| carried.palette != id) {
        drag = None;
    }
    // Where a drop would land, as `Drag::aim` left it at the end of the *last*
    // frame — one frame behind the pointer, which nobody can see in a drag, and
    // it is what lets the ring be drawn as the cell is drawn rather than in a
    // second pass over the grid.
    let aimed = drag.as_ref().and_then(swatchdrag::Drag::destination);
    let dragging = drag.is_some();
    let (pointer, origin, down, released, deciding) = ui.input(|i| {
        (
            i.pointer.interact_pos(),
            i.pointer.press_origin(),
            i.pointer.primary_down(),
            i.pointer.any_released(),
            i.pointer.is_decidedly_dragging(),
        )
    });
    // Collected only while a button is down or something is already carried:
    // the grid is redrawn every frame and this would otherwise be a `Vec` built
    // sixty times a second to answer a question nobody is asking.
    let watching = dragging || down;
    let mut cells: Vec<swatchdrag::Cell> = Vec::new();
    // The colour whose name is being typed keeps its mark while the field is
    // open, so there is something on the grid saying which one the field below
    // belongs to.
    let named = state
        .naming
        .as_ref()
        .filter(|naming| naming.palette == id)
        .map(|naming| naming.index);

    let width = ui.available_width();
    let columns = grid_columns(width, palette.columns);
    let rows = palette.len().div_ceil(columns);
    let step = metrics::PALETTE_SWATCH + metrics::PALETTE_SWATCH_GAP;
    let (area, _) = ui.allocate_exact_size(
        vec2(
            width.max(metrics::PALETTE_SWATCH),
            rows as f32 * step - metrics::PALETTE_SWATCH_GAP,
        ),
        Sense::hover(),
    );

    // The colour in hand, so the swatch it came from can say so. Compared as
    // the bytes a swatch holds rather than as a linear colour: a palette is
    // eight-bit sRGB, and comparing floats would miss by a rounding step.
    let [r, g, b, _] = ed.color.to_srgb_u8();
    let in_hand = [r, g, b];

    let mut act = None;
    for (index, swatch) in palette.swatches.iter().enumerate() {
        let cell = swatch_rect(area.min, index, columns);
        // A palette may hold `MAX_SWATCHES`, and this loop does two `interact`s
        // and two shapes per colour — four thousand of each, every frame, for a
        // panel showing forty. The brush list skips rows scrolled out of view
        // for exactly this reason. A swatch wholly outside the clip is neither
        // drawn nor a target, so leaving it out changes nothing anybody can
        // point at.
        if !ui.is_rect_visible(cell) {
            continue;
        }
        // A culled swatch is not a drop target either, which is the honest
        // answer: it is neither drawn nor clickable, so it must not be
        // something a ring could appear on off screen.
        if watching {
            cells.push(swatchdrag::Cell { index, rect: cell });
        }
        let response = ui.interact(cell, ui.id().with(("swatch", index)), Sense::click());
        // `contains_pointer`, which is geometry and is layer- and clip-aware —
        // not `response.hovered()`. The remove mark below is an interactive
        // widget on top of this one and egui stops its hover search at the
        // topmost, so a mark drawn only while the swatch reported *hovered*
        // would blink out the moment the pointer reached it and back in the
        // frame after. See the module docs.
        let pointer_inside = response.contains_pointer();

        let fill = Color32::from_rgb(swatch.rgb[0], swatch.rgb[1], swatch.rgb[2]);
        ui.painter().rect_filled(cell, metrics::RADIUS, fill);
        // Always an outline, or a swatch the colour of the panel has no edge at
        // all — which on a light theme is most of the top of a greyscale ramp.
        let outline = if swatch.rgb == in_hand {
            Stroke::new(2.0, p.accent)
        } else {
            Stroke::new(1.0, p.border)
        };
        ui.painter()
            .rect_stroke(cell, metrics::RADIUS, outline, StrokeKind::Inside);

        // Round the outside, in the gap, so the colour is never painted over.
        if aimed == Some(index) {
            drop_ring(ui.painter(), p, cell);
        }

        // Both marks are allocated whether or not they are drawn, so their ids
        // and their rectangles do not appear and disappear between frames.
        let mark = remove_rect(cell);
        let remove = ui.interact(mark, ui.id().with(("swatch-remove", index)), Sense::click());
        let naming = name_rect(cell);
        let name = ui.interact(naming, ui.id().with(("swatch-name", index)), Sense::click());
        // Not while a colour is being carried: the marks sit under the pointer
        // for the whole of a drag, and two chips flickering over the grid say
        // nothing about where the colour is going.
        let revealed = pointer_inside && !dragging;
        if revealed {
            ui.painter().rect_filled(mark, metrics::RADIUS, p.popover);
            crate::icons::draw(
                ui.painter(),
                mark.shrink(2.0),
                Icon::Close,
                if remove.hovered() {
                    p.warning
                } else {
                    p.text_dim
                },
            );
        }
        // The name mark alone stays out while the field below is open, so there
        // is something on the grid saying which colour that field belongs to.
        // **The remove mark deliberately does not**: it is destructive, there is
        // no undo for a palette, and leaving it standing would park a live
        // delete one pixel-slip from the field somebody is typing into. A mark
        // that is only there to identify a colour has no business being the one
        // that can throw it away.
        if revealed || named == Some(index) {
            ui.painter().rect_filled(naming, metrics::RADIUS, p.popover);
            crate::icons::draw(
                ui.painter(),
                naming.shrink(2.0),
                Icon::Rename,
                if name.hovered() || named == Some(index) {
                    p.accent
                } else {
                    p.text_dim
                },
            );
        }

        // A release that ends a drag is not also a click on the colour it
        // landed on, nor on whichever mark it happened to pass over. egui
        // settles most of this already — `clicked` does not fire once a press
        // is decidedly a drag — and this is the half that egui cannot know:
        // three widgets share these pixels and the drag belongs to none of
        // them.
        if dragging {
            continue;
        }

        if pointer_inside {
            // The label is built here and not above the hover test: `hex` is a
            // `format!`, and one allocation per swatch per frame is the thing
            // the cull above exists to avoid, done a second way.
            let label = if swatch.name.trim().is_empty() {
                swatch.hex()
            } else {
                format!("{} · {}", swatch.name, swatch.hex())
            };
            if remove.on_hover_text("Remove this colour").clicked() {
                act = Some(Act::Remove(index));
            } else if name.on_hover_text("Name this colour").clicked() {
                act = Some(Act::Name(index));
            } else if response.on_hover_text(label).clicked() {
                act = Some(Act::Take(index));
            }
        } else if response.clicked() {
            // Reachable without the pointer: a click delivered by the keyboard,
            // or one whose press landed here and whose release did not.
            act = Some(Act::Take(index));
        }
    }

    // Picking a colour up, aiming it and putting it down. Off the pointer's own
    // state rather than a `Response`, exactly as the layer list's is and for the
    // same reason: the swatch senses clicks only, and a second widget laid over
    // it to sense drags would be on top of the two corner marks and leave both
    // dead.
    if drag.is_none()
        && down
        && deciding
        && let Some(index) = origin.and_then(|at| drag_origin(&cells, at))
    {
        drag = Some(swatchdrag::Drag::new(id, index));
    }
    if let Some(carried) = &mut drag {
        let from = carried.from;
        // The legality is the model's, asked rather than restated — which is
        // also what refuses the drop that would move nothing.
        carried.aim(&cells, pointer, metrics::PALETTE_SWATCH_GAP, |to| {
            palette.can_move_swatch(from, to)
        });
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
    }
    if !down && let Some(carried) = drag.take() {
        // `released` distinguishes the frame the button came up on from a drag
        // left in the store by a panel that stopped being drawn mid-gesture.
        // Without it, reopening the module with the pointer over the grid would
        // resolve a drop nobody was making.
        if released && let Some(to) = carried.destination() {
            act = Some(Act::Move {
                from: carried.from,
                to,
            });
        }
    }
    ui.ctx().data_mut(|d| match drag {
        Some(drag) => {
            d.insert_temp(drag_id(), drag);
        }
        None => d.remove::<swatchdrag::Drag>(drag_id()),
    });

    act
}

/// What the panel shows with no palette to show.
fn empty_library(ui: &mut Ui, p: &Palette, ed: &mut Editor, state: &mut State) {
    controls::note(
        ui,
        p,
        "No palettes yet. Make one and it fills with the colours you save into \
         it, or bring one in. Umber writes GIMP's .gpl, which GIMP, Krita, \
         Inkscape and Aseprite all read, and it opens palettes from Coolors, \
         Lospec, Adobe Color, Photoshop and Paint.NET.",
    );
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if controls::text_button(ui, p, "New palette", true, true).clicked() {
            new_palette(state, ed);
        }
        if controls::text_button(ui, p, "Import…", false, true).clicked() {
            import(state, ed);
        }
        // Straight into the library with the pane open, rather than a second
        // paste field in the panel. There is one of this control, in the modal
        // where making a palette already lives.
        if controls::text_button(ui, p, "Paste…", false, true)
            .on_hover_text("Paste hex codes, a Coolors link or a block of CSS")
            .clicked()
        {
            state.library_open = true;
            state.pasting = Some(Pasting {
                focus: true,
                ..Pasting::default()
            });
        }
    });
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Change the palette in front, and write it out — **if it changed**.
///
/// One door, so every edit reads the palette, changes it and saves it in the
/// same order, and so the write's failure is reported once rather than at five
/// call sites.
///
/// `change` answers whether it did anything, and a `false` writes nothing. That
/// is not an optimisation: a `PaletteLibrary` write reaches the disk on the
/// spot, and every one of the five model methods behind this already returns
/// exactly that answer — a full palette refusing a colour, a move the model
/// declined, a name typed identical to the one already there, a field that lost
/// focus with nothing typed into it. Ignoring the answer meant pressing Enter
/// on an unchanged name rewrote the artist's file. It also lets the callers
/// stop guarding by hand: `keep_harmony` had a second room check for precisely
/// this reason and no longer needs one.
fn edit_current(
    state: &mut State,
    ed: &mut Editor,
    what: &str,
    change: impl FnOnce(&mut ColourPalette) -> bool,
) {
    let Some(mut palette) = state.current().cloned() else {
        return;
    };
    if !change(&mut palette) {
        return;
    }
    write(state, ed, what, |library| library.save(palette));
}

fn add_current_colour(state: &mut State, ed: &mut Editor) {
    let swatch = Swatch::of(ed.color);
    edit_current(state, ed, "Could not save the palette", |palette| {
        palette.add(swatch)
    });
}

/// Put the harmony of the colour in hand into the palette in front.
///
/// **The whole set or none of it** — `Palette::add_all` is the rule and the
/// argument, and there is deliberately no second gate here. The mark above is
/// disabled where the set will not fit, and where the palette has changed
/// between that frame and the click `add_all` refuses and [`edit_current`]
/// writes nothing, because it takes the answer rather than assuming one.
///
/// **A colour already in the palette goes in again**, including the base, which
/// is always a member — `Harmony::offsets` puts the hue in hand first. Pressing
/// the mark twice therefore puts the set in twice. That is `Palette::add`'s own
/// rule and it is right for the same reason: a duplicate is something the
/// artist can see and take out, where a control that silently did less than it
/// said is something they would have to work out.
///
/// Built from the picker's own [`Hsv`] and not from `Editor::color`, **including
/// the member that is the colour in hand**. A harmony is a set of hues at one
/// saturation and value — `umber_core::harmony` has the argument — so taking
/// the base from anywhere else would put a colour in the set that is not on the
/// same wheel as the rest of it, and would not be the colour the Colour panel's
/// own harmony row is showing. That is also why the hue comes off `Editor::hsv`
/// rather than off the colour: hue is undefined for a grey, and a harmony read
/// off the colour would be a red one for every grey.
///
/// Each hue is turned into eight-bit sRGB once, on the way in. Nothing here
/// takes a swatch back out through [`umber_core::Color`].
fn keep_harmony(state: &mut State, ed: &mut Editor) {
    let hsv = ed.hsv;
    let swatches: Vec<Swatch> = ed
        .ui
        .harmony
        .hues(hsv.h)
        .as_slice()
        .iter()
        .map(|hue| Swatch::of(Hsv::new(*hue, hsv.s, hsv.v).to_color(1.0)))
        .collect();
    edit_current(state, ed, "Could not save the palette", |palette| {
        palette.add_all(&swatches)
    });
}

/// Make a palette out of whatever was pasted.
///
/// The colours are the ones the readout was drawn from, not a fresh parse of
/// the field: re-reading here would let the button add a palette the line above
/// it did not describe, which is the lying control this codebase refuses
/// everywhere.
///
/// Through [`PaletteLibrary::adopt`] — the same door an imported file goes
/// through — so a paste lands as a new file with a name nothing else is called,
/// rather than quietly replacing whatever happens to be called "Pasted
/// colours" already.
fn add_pasted(state: &mut State, ed: &mut Editor) {
    let Some(pasting) = state.pasting.clone() else {
        return;
    };
    if pasting.found.is_empty() {
        return;
    }
    let mut palette = ColourPalette::new(PASTED_NAME);
    palette.swatches = pasting.found;
    let Some(id) = write(state, ed, "Could not save the palette", |library| {
        library.adopt(palette)
    }) else {
        return;
    };
    state.selected = Some(id);
    state.pasting = None;
    // Named once, after the fact, and only where something was actually lost.
    // The pane already showed these while the text sat there; the notice is for
    // somebody who pressed the button without reading it.
    if pasting.losses.any() {
        ed.notice = Some(Notice {
            title: "Added, with notes".to_owned(),
            lines: pasting.losses.sentences(),
        });
    }
}

/// The field somebody pastes into, and what Umber makes of it.
///
/// **A field rather than a button that reads the clipboard**, and that is
/// decided rather than lazy. `arboard` is already here for the canvas, so a
/// one-click "paste colours" was available — and it would have been a control
/// that reaches into the system clipboard and makes a file out of whatever it
/// found, with nothing on screen between the two. A field shows what is about
/// to be read, can be corrected, takes a link typed by hand as happily as one
/// pasted, and needs no clipboard code at all: `egui-winit`'s `clipboard`
/// feature already maps Ctrl+V into a `TextEdit`. It also keeps `sysclip`'s
/// standing rule free — no test here can touch the real clipboard, because
/// nothing here reads one.
///
/// It needs no `shortcuts::set_capturing`: `ui::draw` calls
/// `shortcuts::set_typing(ctx.text_edit_focused())` once for the whole
/// interface and a real `TextEdit` is covered by it.
fn paste_pane(ui: &mut Ui, p: &Palette, ed: &mut Editor, state: &mut State) {
    let Some(pasting) = state.pasting.as_mut() else {
        return;
    };
    // What the pane decided this frame, collected because acting on it needs
    // the state the field is borrowing. The same arrangement `naming_field`
    // keeps.
    let mut add = false;
    let mut close = false;

    Frame::NONE
        .fill(p.window)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(metrics::RADIUS_LARGE)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            controls::note(
                ui,
                p,
                "Paste a list of hex codes, a Coolors link, a block of CSS, or \
                 anything else with colours in it. Umber reads #RRGGBB, #RGB, \
                 0xRRGGBB and rgb(...), and takes a name from the line where \
                 there is one.",
            );
            ui.add_space(6.0);
            let field = ui.add(
                egui::TextEdit::multiline(&mut pasting.text)
                    .desired_width(ui.available_width())
                    .desired_rows(PASTE_ROWS)
                    .hint_text("#10121c, #2c1e31, #6b2643")
                    .font(egui::FontId::monospace(text::SMALL)),
            );
            if pasting.focus {
                field.request_focus();
                pasting.focus = false;
            }
            // Once per edit, never once per frame: the text may be megabytes
            // and this walks all of it.
            if field.changed() {
                pasting.reread();
            }

            ui.add_space(6.0);
            match (&pasting.refusal, pasting.found.len()) {
                (Some(why), _) => controls::note(ui, p, why),
                (None, 0) if pasting.text.trim().is_empty() => {}
                // Not a failure and not phrased as one: an artist who has
                // pasted the wrong thing needs to know what would have worked.
                (None, 0) => controls::note(
                    ui,
                    p,
                    "No colours in that yet. A list like #10121c, #2c1e31 works, \
                     and so does a link such as coolors.co/10121c-2c1e31.",
                ),
                (None, count) => {
                    ui.label(
                        egui::RichText::new(format!(
                            "{count} colour{} found",
                            if count == 1 { "" } else { "s" }
                        ))
                        .size(text::SMALL)
                        .color(p.text_strong),
                    );
                    // Said here rather than only after the fact, so a loss can
                    // be reconsidered while the text is still on screen.
                    for sentence in pasting.losses.sentences() {
                        controls::note(ui, p, &sentence);
                    }
                }
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let ready = pasting.refusal.is_none() && !pasting.found.is_empty();
                if controls::text_button(ui, p, "Add as a palette", true, ready)
                    .on_hover_text(if ready {
                        "Put these colours in your library as a new palette"
                    } else {
                        "Paste something with colours in it first"
                    })
                    .clicked()
                {
                    add = true;
                }
                if controls::text_button(ui, p, "Cancel", false, true).clicked() {
                    close = true;
                }
            });
        });

    if close {
        state.pasting = None;
    }
    if add {
        add_pasted(state, ed);
    }
}

fn new_palette(state: &mut State, ed: &mut Editor) {
    if let Some(id) = write(state, ed, "Could not make a palette", |library| {
        library.create("My palette")
    }) {
        // Selected on the spot: somebody who has just made a palette wants to
        // put a colour in it, not to go and find it in a list.
        state.selected = Some(id);
    }
}

/// Bring in one or more palette files, in any format Umber reads.
///
/// Several at once because a palette collection *is* a folder of them, which is
/// the same reason the brush importer takes several files.
///
/// The filter list is built from [`palimport::Format::ALL`] rather than typed
/// here, so a format added to the model reaches this dialog and cannot be a
/// reader nobody can get a file to. One combined filter first, because the
/// ordinary case is "show me my palettes" and not "I know this one is an
/// `.ase`"; the per-format rows are under it for somebody sorting a folder of
/// mixed files.
fn import(state: &mut State, ed: &mut Editor) {
    if !state.writable() {
        return;
    }
    let every: Vec<&str> = palimport::Format::ALL
        .iter()
        .map(|format| format.extension())
        .collect();
    let mut dialog = rfd::FileDialog::new()
        .set_title("Import palettes")
        .add_filter("Palettes", &every);
    for format in palimport::Format::ALL {
        dialog = dialog.add_filter(format.label(), &[format.extension()]);
    }
    // Deliberately present and deliberately last: picking the wrong kind of
    // file gives a sentence naming it and the reason, which is a better
    // answer than a picker that refuses to show the file at all.
    let Some(paths) = dialog.add_filter("All files", &["*"]).pick_files() else {
        return;
    };

    let mut added = None;
    let mut lines = Vec::new();
    for path in &paths {
        let Store::Ready(library) = &mut state.store else {
            return;
        };
        match Arc::make_mut(library).import(path) {
            Ok((id, losses)) => {
                // An import that loses something says so, and the sentences are
                // the model's rather than this file's — one statement of what a
                // dropped colour space or a skipped entry reads as, wherever it
                // is reported. Named with the file only where there were
                // several, since with one the prefix is the same word on every
                // line.
                for sentence in losses.sentences() {
                    lines.push(if paths.len() > 1 {
                        format!("{}: {sentence}", file_label(path))
                    } else {
                        sentence
                    });
                }
                added = Some(id);
            }
            Err(e) => lines.push(e.to_string()),
        }
    }
    state.settle_selection();
    let any = added.is_some();
    if let Some(id) = added {
        state.selected = Some(id);
    }
    if !lines.is_empty() {
        ed.notice = Some(Notice {
            // Both halves are reachable in one go: a folder of palettes may hold
            // one file that reads and one that does not, and the title has to
            // say which of the two happened rather than only the last.
            title: if any {
                "Imported, with notes".to_owned()
            } else {
                "Could not import".to_owned()
            },
            lines,
        });
    }
}

fn export(state: &mut State, ed: &mut Editor, id: &str) {
    let Some(library) = state.library().cloned() else {
        return;
    };
    let Some(palette) = library.get(id) else {
        return;
    };
    let Some(path) = rfd::FileDialog::new()
        .set_title("Export palette")
        .add_filter("GIMP palette", &[GPL_EXTENSION])
        // The *id*, not the display name: an id is already a filename — see
        // `PaletteLibrary`'s `slug` — and a name may hold a separator or a
        // colon, which is a file dialog opened on a path nobody meant.
        .set_file_name(format!("{}.{GPL_EXTENSION}", palette.id))
        .save_file()
    else {
        return;
    };
    if let Err(e) = library.export(id, &path) {
        ed.notice = Some(Notice {
            title: "Could not export the palette".to_owned(),
            lines: vec![e.to_string()],
        });
    }
}

/// A path as a sentence names it: the filename, or the whole path if it has
/// none.
fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

// ---------------------------------------------------------------------------
// The library modal
// ---------------------------------------------------------------------------

/// The palette library. Drawn from [`crate::panels::sidebars`], not from the
/// panel body — see the module docs.
pub fn dialogs(root: &mut Ui, p: &Palette, ed: &mut Editor) {
    // Nothing at all until the module is in the layout or the state already
    // exists. `sidebars` calls this every frame whether or not the Palette
    // panel is anywhere, and `load` reads the whole directory — up to
    // `MAX_PALETTES` files, synchronously, inside a frame — and can raise a
    // notice about a file that would not parse. Doing either at launch for
    // somebody who has never opened the panel is a dialog about a feature they
    // have not asked for. Once the state is in memory this costs a lookup, so
    // closing the module mid-session does not throw the library away.
    if !ed.layout.is_open(crate::dock::PanelKind::Palette)
        && !root
            .ctx()
            .data(|d| d.get_temp::<State>(state_id()).is_some())
    {
        return;
    }
    let mut state = load(root.ctx(), ed);
    // This runs every frame whether or not the panel is in the layout, which
    // makes it the one place that can see the module leave. A gesture left
    // half-made cannot survive that: see `forget_gesture` for what an open
    // naming field does when its widget stops being drawn.
    if !ed.layout.is_open(crate::dock::PanelKind::Palette) {
        forget_gesture(root.ctx(), &mut state);
    }
    if !state.library_open {
        store(root.ctx(), state);
        return;
    }

    // Clamped to the window, for the reason `brushlib::browser` clamps: a modal
    // wider than the screen has no way back out of its own corners, and the
    // Close mark is in one of them.
    let available = root.ctx().content_rect().size();
    let [full_width, full_height] = metrics::PALETTE_LIBRARY;
    let w = full_width.min(available.x - 48.0).max(280.0);
    let mut h = full_height.min(available.y - 220.0).max(120.0);
    // The pane goes *above* the list, so its room comes out of the list's
    // rather than out of the window's. A modal that grew by a hundred and fifty
    // points when a control opened would push its own Close mark off the
    // bottom, which is the corner `brushlib::browser`'s clamp exists to keep
    // reachable.
    if state.pasting.is_some() {
        h = (h - PASTE_BLOCK).max(80.0);
    }

    let response = egui::Modal::new(Id::new("palette-library-modal"))
        .frame(
            Frame::NONE
                .fill(p.popover)
                .stroke(Stroke::new(1.0, p.popover_border))
                .corner_radius(8)
                .inner_margin(egui::Margin::same(18)),
        )
        .show(root.ctx(), |ui| {
            ui.set_width(w);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Palettes")
                        .size(text::CONTROL)
                        .color(p.text_strong)
                        .strong(),
                );
                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon_button(ui, p, Icon::Close, true, "Close") {
                        state.library_open = false;
                    }
                });
            });
            // The list of extensions is `readable_formats()`'s and not a second
            // hand-written one twenty lines from the Import tooltip that calls
            // it: a seventh format would otherwise update one and not the
            // other, and this is the copy an artist reads.
            controls::note(
                ui,
                p,
                &format!(
                    "Every palette is kept as one .gpl file in a folder of its \
                     own, which is the format GIMP, Krita, Inkscape and \
                     Aseprite all read. Import takes {}, so a palette from \
                     Coolors, Lospec, Adobe Color, Photoshop or Paint.NET \
                     comes straight in.",
                    palimport::readable_formats()
                ),
            );
            ui.add_space(10.0);

            ui.horizontal(|ui| {
                // Room is what stops a palette being written that the next
                // launch would not read back — see `PaletteLibrary::save`. The
                // control is disabled and says which of the two it is rather
                // than being live and refusing.
                let room = state.library().is_some_and(|library| library.has_room());
                let can_make = state.writable() && room;
                if controls::text_button(ui, p, "New palette", true, can_make)
                    .on_hover_text(if can_make {
                        "Make an empty palette to save colours into".to_owned()
                    } else if !state.writable() {
                        state.why_not().to_owned()
                    } else {
                        format!(
                            "Your library already holds {} palettes",
                            palette::MAX_PALETTES
                        )
                    })
                    .clicked()
                {
                    new_palette(&mut state, ed);
                }
                if controls::text_button(ui, p, "Import…", false, can_make)
                    .on_hover_text(if can_make {
                        format!(
                            "Bring a palette file into your library. Umber reads {}.",
                            palimport::readable_formats()
                        )
                    } else if !state.writable() {
                        state.why_not().to_owned()
                    } else {
                        format!(
                            "Your library already holds {} palettes",
                            palette::MAX_PALETTES
                        )
                    })
                    .clicked()
                {
                    import(&mut state, ed);
                }
                // The commonest palette in the world is a list of hex codes
                // somebody copied off a page, and no file format reaches it.
                // See `paste_pane` for why this opens a field rather than
                // reading the clipboard on the spot.
                if controls::text_button(ui, p, "Paste colours…", false, can_make)
                    .on_hover_text(if can_make {
                        "Paste hex codes, a Coolors link or a block of CSS".to_owned()
                    } else if !state.writable() {
                        state.why_not().to_owned()
                    } else {
                        format!(
                            "Your library already holds {} palettes",
                            palette::MAX_PALETTES
                        )
                    })
                    .clicked()
                {
                    state.pasting = Some(Pasting {
                        focus: true,
                        ..Pasting::default()
                    });
                }
                if let Some(dir) = state.library().map(|library| library.dir().to_path_buf()) {
                    ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                        folder_link(ui, p, &dir);
                    });
                }
            });
            ui.add_space(8.0);

            // With no library there is nothing to list and two dead buttons
            // above, so the modal has to say what is wrong rather than being a
            // note about .gpl and a rectangle of nothing. The panel body says
            // the same sentence for the same reason.
            if !state.writable() {
                controls::note(ui, p, state.why_not());
            }

            if state.pasting.is_some() {
                paste_pane(ui, p, ed, &mut state);
                ui.add_space(8.0);
            }

            egui::ScrollArea::vertical()
                .id_salt("palette-library-list")
                .auto_shrink([false, false])
                .max_height(h)
                .show(ui, |ui| library_list(ui, p, ed, &mut state));
        });

    if response.should_close() {
        state.library_open = false;
    }
    if !state.library_open {
        // A field left open would take the keyboard back the next time the
        // modal is raised, over whichever palette happened to be selected then.
        state.renaming = None;
        state.confirming = None;
        // And the paste pane goes with them, for the reason `forget_gesture`
        // gives about the naming field: the widget stops being drawn, so egui
        // drops its focus without it ever reporting `lost_focus`, and text left
        // in it would come back beside a readout nobody could see it produce.
        state.pasting = None;
    }
    store(root.ctx(), state);
}

/// Where the files are, so somebody can go and look at them.
///
/// `autosave::reveal` rather than a second `explorer`/`open`/`xdg-open` match:
/// there is one way Umber shows a folder, and the awkward part — that Explorer
/// reports failure on success, so the exit status may not be read — is stated
/// there once.
fn folder_link(ui: &mut Ui, p: &Palette, dir: &Path) {
    let label = ui.add(
        egui::Label::new(
            egui::RichText::new("Open folder")
                .size(text::TINY)
                .color(p.text_dim),
        )
        .sense(Sense::click()),
    );
    // A failure is a log line and nothing else: not being able to open a folder
    // must never raise a dialog over somebody's canvas, and the path is already
    // on screen in the tooltip.
    if label.on_hover_text(dir.display().to_string()).clicked()
        && let Err(e) = crate::autosave::reveal(dir)
    {
        log::warn!("could not open {}: {e}", dir.display());
    }
}

fn library_list(ui: &mut Ui, p: &Palette, ed: &mut Editor, state: &mut State) {
    let Some(library) = state.library().cloned() else {
        return;
    };
    if library.is_empty() {
        controls::note(ui, p, "Nothing here yet.");
        return;
    }

    // Collected and applied below the loop, because every arm needs the state
    // mutably in a way the loop cannot — a delete resolves against a library
    // the rows are still being drawn from.
    //
    // **Two slots, not one.** A field losing focus and a button being pressed
    // are one frame's worth of *different* events: clicking Rename on a row
    // below one whose field is open fires both, and a single "last one wins"
    // slot threw the typed name away with nothing to show for it. The rename
    // lands first below, so the name is saved and the button still does what it
    // was pressed for.
    let mut request = None;
    let mut renamed = None;
    for palette in library.palettes() {
        match library_row(ui, p, state, palette) {
            Some(Request::Rename(name)) => renamed = Some((palette.id.clone(), name)),
            Some(Request::CancelRename) => renamed = Some((palette.id.clone(), String::new())),
            Some(asked) => request = Some((palette.id.clone(), asked)),
            None => {}
        }
        ui.add_space(6.0);
    }

    if let Some((id, name)) = renamed {
        state.renaming = None;
        // An empty name is how the loop above spells Escape: the model would
        // substitute "Untitled palette" for one, so it cannot be a real name
        // arriving here and there is nothing to disambiguate.
        if !name.is_empty() {
            write(state, ed, "Could not rename the palette", |library| {
                library.rename(&id, &name)
            });
        }
    }

    match request {
        Some((id, Request::Select)) => state.selected = Some(id),
        Some((id, Request::StartRename)) => {
            let text = library.get(&id).map(|q| q.name.clone()).unwrap_or_default();
            state.renaming = Some(Renaming {
                id,
                text,
                focus: true,
            });
        }
        Some((id, Request::Confirm)) => state.confirming = Some(id),
        Some((_, Request::Keep)) => state.confirming = None,
        Some((id, Request::Export)) => export(state, ed, &id),
        Some((id, Request::Delete)) => {
            state.confirming = None;
            write(state, ed, "Could not delete the palette", |library| {
                library.remove(&id)
            });
        }
        Some((_, Request::Rename(_) | Request::CancelRename)) | None => {}
    }
}

enum Request {
    Select,
    StartRename,
    Rename(String),
    CancelRename,
    Export,
    /// The first press of Delete: arm it and say so.
    Confirm,
    /// Put an armed Delete back. `brushlib::confirm_overlay` spells this
    /// **Keep**, so this does too — the alternative was a Delete that read
    /// "Really?" with no way back except closing the modal, which leaves a row
    /// armed and forgotten and deletes it on one click minutes later.
    Keep,
    Delete,
}

/// The strip of colours on a row, and how many of them it draws.
///
/// A bounded number, because a palette may hold thousands and a row is one
/// line: the strip says what kind of palette this is — warm, grey, garish — and
/// forty tiles say that as well as four thousand would, at a cost that does not
/// grow with the file.
const STRIP_TILES: usize = 40;
const STRIP_HEIGHT: f32 = 14.0;

fn library_row(
    ui: &mut Ui,
    p: &Palette,
    state: &mut State,
    palette: &ColourPalette,
) -> Option<Request> {
    let selected = state.selected.as_deref() == Some(palette.id.as_str());
    let renaming = state.renaming.as_ref().is_some_and(|r| r.id == palette.id);
    let confirming = state.confirming.as_deref() == Some(palette.id.as_str());

    let mut request = None;
    Frame::NONE
        .fill(if selected { p.control } else { p.window })
        .stroke(Stroke::new(1.0, if selected { p.accent } else { p.border }))
        .corner_radius(metrics::RADIUS_LARGE)
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                // The buffer is the state's own, edited in place: a `TextEdit`'s
                // text belongs to the caller, and the row is rebuilt every
                // frame, so a local copy would lose a character per frame.
                match state.renaming.as_mut().filter(|_| renaming) {
                    Some(rename) => {
                        let field = ui.add(
                            egui::TextEdit::singleline(&mut rename.text)
                                .desired_width(160.0)
                                .font(egui::FontId::proportional(text::SMALL)),
                        );
                        if rename.focus {
                            field.request_focus();
                            rename.focus = false;
                        }
                        if field.lost_focus() {
                            // Escape abandons; anything else — Enter, or a click
                            // elsewhere — keeps what was typed. A field cleared
                            // and blurred is not a nameless palette: the model
                            // *substitutes* "Untitled palette" for an empty
                            // name rather than refusing it, so the worst case is
                            // a palette called that.
                            let typed = rename.text.trim().to_owned();
                            request = Some(
                                if typed.is_empty()
                                    || ui.input(|i| i.key_pressed(egui::Key::Escape))
                                {
                                    Request::CancelRename
                                } else {
                                    Request::Rename(typed)
                                },
                            );
                        }
                    }
                    None => {
                        let name = ui.add(
                            egui::Label::new(
                                egui::RichText::new(&palette.name)
                                    .size(text::SMALL)
                                    .color(p.text_strong),
                            )
                            .sense(Sense::click()),
                        );
                        if name
                            .on_hover_text("Show this palette in the panel")
                            .clicked()
                        {
                            request = Some(Request::Select);
                        }
                    }
                }
                ui.label(
                    egui::RichText::new(format!("{}", palette.len()))
                        .monospace()
                        .size(text::TINY)
                        .color(p.text_dim),
                );

                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    // An armed Delete shows a **Delete/Keep** pair, the way
                    // `brushlib::confirm_overlay` asks. A single button that
                    // changed its own label to "Really?" had no way back: a row
                    // armed and forgotten deletes on one click minutes later,
                    // and the only escape was closing the modal.
                    if confirming {
                        if controls::text_button(ui, p, "Delete", true, true)
                            .on_hover_text(
                                "This cannot be undone. The history covers painting only.",
                            )
                            .clicked()
                        {
                            request = Some(Request::Delete);
                        }
                        if controls::text_button(ui, p, "Keep", false, true)
                            .on_hover_text("Leave this palette where it is")
                            .clicked()
                        {
                            request = Some(Request::Keep);
                        }
                    } else {
                        if controls::text_button(ui, p, "Delete", false, true)
                            .on_hover_text("Delete this palette")
                            .clicked()
                        {
                            request = Some(Request::Confirm);
                        }
                        if controls::text_button(ui, p, "Export…", false, !palette.is_empty())
                            .on_hover_text(if palette.is_empty() {
                                "Nothing to export yet"
                            } else {
                                "Write this palette out as a .gpl"
                            })
                            .clicked()
                        {
                            request = Some(Request::Export);
                        }
                        if controls::text_button(ui, p, "Rename", false, true)
                            .on_hover_text("Give this palette another name")
                            .clicked()
                        {
                            request = Some(Request::StartRename);
                        }
                    }
                });
            });
            ui.add_space(6.0);
            colour_strip(ui, p, palette);
        });
    request
}

/// A palette's colours as a single band, so a row can be told from its
/// neighbours at a glance.
fn colour_strip(ui: &mut Ui, p: &Palette, palette: &ColourPalette) {
    let width = ui.available_width().max(1.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, STRIP_HEIGHT), Sense::hover());
    // The room is claimed either way, so the list does not change height as it
    // scrolls; only the forty-odd shapes are skipped. A library may hold
    // `MAX_PALETTES` rows, which is the same omission the swatch grid just had
    // fixed, one modal away.
    if !ui.is_rect_visible(rect) {
        return;
    }
    let shown = palette.len().min(STRIP_TILES);
    let each = rect.width() / shown.max(1) as f32;
    for (index, swatch) in palette.swatches.iter().take(shown).enumerate() {
        let cell = Rect::from_min_size(
            pos2(rect.left() + index as f32 * each, rect.top()),
            vec2(each, rect.height()),
        );
        ui.painter().rect_filled(
            cell,
            0.0,
            Color32::from_rgb(swatch.rgb[0], swatch.rgb[1], swatch.rgb[2]),
        );
    }
    // Square corners, deliberately, where every other box here is rounded. The
    // tiles are square and the outline is drawn over them, so a corner radius
    // would leave the first and last tile poking out past the arc — egui has no
    // way to clip a painter to a rounded rectangle, and rounding the end tiles
    // instead would be two more cases to keep in step with the strip's width.
    ui.painter()
        .rect_stroke(rect, 0.0, Stroke::new(1.0, p.border), StrokeKind::Inside);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The module's headline rule, in the model: the palette in front is held
    /// by **id**, so a delete cannot silently leave the panel showing a
    /// different palette. It is pure state and needs no window.
    #[test]
    fn the_palette_in_front_is_a_name_and_never_a_position() {
        let dir = std::env::temp_dir().join(format!("umber-palette-settle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut library = PaletteLibrary::load_from(&dir);
        // Named so the sorted order is a, b, c.
        let a = library.create("Alpha").expect("made");
        let b = library.create("Beta").expect("made");
        let c = library.create("Gamma").expect("made");

        let mut state = State {
            store: Store::Ready(Arc::new(library.clone())),
            selected: None,
            library_open: false,
            renaming: None,
            naming: None,
            pasting: None,
            confirming: None,
        };
        // Nothing selected takes the first, so a library with palettes in it
        // never draws a panel that looks broken.
        state.settle_selection();
        assert_eq!(state.selected.as_deref(), Some(a.as_str()));

        // A selection that still names something is left exactly alone, whatever
        // position it is in — this is the case an index would get wrong.
        state.selected = Some(c.clone());
        library.remove(&a).expect("deleted");
        state.store = Store::Ready(Arc::new(library.clone()));
        state.settle_selection();
        assert_eq!(
            state.selected.as_deref(),
            Some(c.as_str()),
            "deleting the palette above it moved the selection"
        );

        // And one whose palette has gone falls back rather than pointing at
        // nothing.
        library.remove(&c).expect("deleted");
        state.store = Store::Ready(Arc::new(library));
        state.settle_selection();
        assert_eq!(state.selected.as_deref(), Some(b.as_str()));

        // With no library at all there is nothing to point at, and a stale id
        // would outlive the thing it named.
        state.store = Store::Broken("no data directory".into());
        state.settle_selection();
        assert_eq!(state.selected, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The paste, end to end: what the readout says is what the button adds.
    ///
    /// **No test here may touch the real clipboard** — a CI runner may have no
    /// display server, and grabbing somebody's clipboard on their own machine
    /// is hostile. That rule is free here rather than obeyed here, because the
    /// pane is a field: nothing on this path reads a clipboard at all. See
    /// `paste_pane` for why it was built that way.
    #[test]
    fn what_was_pasted_is_what_lands_in_the_library() {
        let dir = std::env::temp_dir().join(format!("umber-palette-paste-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let library = PaletteLibrary::load_from(&dir);
        let mut ed = Editor::default();
        let mut state = State {
            store: Store::Ready(Arc::new(library)),
            selected: None,
            library_open: true,
            renaming: None,
            naming: None,
            pasting: None,
            confirming: None,
        };

        // A Coolors link, which is how a palette actually travels between two
        // people, pasted into a sentence the way one arrives in a chat window.
        let mut pasting = Pasting {
            text: "try https://coolors.co/10121c-2c1e31-6b2643".to_owned(),
            ..Pasting::default()
        };
        pasting.reread();
        assert_eq!(pasting.found.len(), 3, "the readout's own number");
        assert!(pasting.refusal.is_none());
        assert!(!pasting.losses.any(), "an opaque paste loses nothing");
        state.pasting = Some(pasting);

        add_pasted(&mut state, &mut ed);
        assert!(state.pasting.is_none(), "the pane closes once it is used");
        let palette = state.current().expect("selected on the spot");
        assert_eq!(palette.name, PASTED_NAME);
        assert_eq!(
            palette.swatches,
            vec![
                Swatch::new([0x10, 0x12, 0x1c]),
                Swatch::new([0x2c, 0x1e, 0x31]),
                Swatch::new([0x6b, 0x26, 0x43]),
            ]
        );
        // It reached the disk, which is the whole shape of a directory of
        // `.gpl` files: there is no separate save.
        let reopened = PaletteLibrary::load_from(&dir);
        assert_eq!(reopened.palettes().len(), 1);
        assert_eq!(reopened.palettes()[0].swatches, palette.swatches);

        // A second paste lands **beside** the first rather than replacing the
        // palette already called "Pasted colours" — `adopt`'s rule, and the one
        // an artist pasting twice in a row would otherwise lose work to.
        let mut again = Pasting {
            text: "#CC7722".to_owned(),
            ..Pasting::default()
        };
        again.reread();
        state.pasting = Some(again);
        add_pasted(&mut state, &mut ed);
        let library = state.library().expect("still there");
        assert_eq!(library.palettes().len(), 2);
        assert_eq!(library.palettes()[0].swatches.len(), 3);
        assert_eq!(library.palettes()[1].swatches.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A pane with nothing usable in it adds nothing, and the button that would
    /// have done it is dead rather than live and refusing. Three states, and
    /// none of them may write a file: an empty palette in the list is one the
    /// artist has to go and delete.
    #[test]
    fn a_paste_with_no_colours_in_it_writes_nothing() {
        let dir =
            std::env::temp_dir().join(format!("umber-palette-paste-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut ed = Editor::default();
        let mut state = State {
            store: Store::Ready(Arc::new(PaletteLibrary::load_from(&dir))),
            selected: None,
            library_open: true,
            renaming: None,
            naming: None,
            pasting: None,
            confirming: None,
        };
        for text in ["", "   ", "just some words about a facade"] {
            let mut pasting = Pasting {
                text: text.to_owned(),
                ..Pasting::default()
            };
            pasting.reread();
            assert!(pasting.found.is_empty(), "{text:?}");
            assert!(pasting.refusal.is_none(), "nothing typed is not an error");
            state.pasting = Some(pasting);
            add_pasted(&mut state, &mut ed);
            assert!(state.pasting.is_some(), "{text:?}: the pane stayed open");
            assert!(
                state.library().expect("there").is_empty(),
                "{text:?}: it wrote a palette anyway"
            );
        }
        // Past what a palette holds is a refusal with a sentence, not a
        // truncation, and the pane says so rather than adding four thousand and
        // ninety-six of five thousand.
        let mut huge = Pasting::default();
        for n in 0..=umber_core::palette::MAX_SWATCHES {
            huge.text.push_str(&format!("#{:06x}\n", n % 0xffffff));
        }
        huge.reread();
        assert!(huge.found.is_empty());
        let why = huge.refusal.expect("a sentence");
        assert!(why.starts_with(PASTE_SOURCE), "{why}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A loss is named where it happened and nowhere else. Pasting opaque
    /// colours must raise nothing at all — a notice shown every time is one
    /// nobody reads, which costs the losses that matter.
    #[test]
    fn a_paste_names_what_it_lost_and_stays_quiet_otherwise() {
        let mut opaque = Pasting {
            text: "#10121cff\n#2c1e31\n".to_owned(),
            ..Pasting::default()
        };
        opaque.reread();
        assert!(opaque.losses.sentences().is_empty());

        let mut faded = Pasting {
            text: "#10121c80\nrgba(1, 2, 3, 0.5)\n".to_owned(),
            ..Pasting::default()
        };
        faded.reread();
        assert_eq!(faded.losses.transparency, 2);
        assert_eq!(faded.losses.sentences().len(), 1);
        // Drawn in a pane over somebody's canvas, so held to the interface's
        // own rule about how a string reads.
        for sentence in faded.losses.sentences() {
            assert!(!sentence.contains('—'), "{sentence}");
        }
    }

    /// A library in a directory of its own, with two palettes in it, for the
    /// preview shots below to draw.
    #[cfg(debug_assertions)]
    fn staged_library(tag: &str) -> PaletteLibrary {
        let dir = std::env::temp_dir().join(format!("umber-palette-shot-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        let mut library = PaletteLibrary::load_from(&dir);
        let id = library.create("Warm earths").expect("made");
        let mut palette = library.get(&id).expect("there").clone();
        for rgb in [
            [40, 26, 19],
            [92, 55, 31],
            [143, 88, 42],
            [186, 129, 68],
            [214, 173, 116],
            [235, 214, 178],
            [246, 240, 226],
            [24, 40, 46],
            [46, 82, 84],
            [96, 132, 122],
            [163, 176, 148],
            [212, 96, 62],
        ] {
            palette.add(umber_core::Swatch::new(rgb));
        }
        library.save(palette).expect("saved");
        let _ = library.create("Greys");
        library
    }

    /// The Palette module at the panel's real width, and the library modal over
    /// it.
    ///
    /// Written rather than asserted for the reason `layers_panel_preview` is:
    /// what can go wrong in a grid of swatches and a row of four buttons in a
    /// 560 px modal is a *layout*, and no assertion about widgets catches
    /// controls drawn over each other. `docshot::Stage` is the only thing in
    /// the crate that can look at a piece of interface.
    ///
    /// ```sh
    /// cargo test -p umber-app palette_module_preview -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "writes preview PNGs and wants a GPU; run deliberately"]
    #[cfg(debug_assertions)]
    fn palette_module_preview() {
        use crate::dock::{Layout, PanelKind};
        use crate::docshot;

        use egui::{Pos2, Rect, vec2};

        let Some(mut stage) = docshot::Stage::new() else {
            eprintln!("no GPU adapter: nothing to draw into. Skipped.");
            return;
        };
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/palette-module");
        std::fs::create_dir_all(&dir).expect("create the preview directory");

        for (name, open, empty, naming, dragging, pasting) in [
            ("1-panel", false, false, false, false, false),
            ("2-panel-empty", false, true, false, false, false),
            ("3-library", true, false, false, false, false),
            // The naming field, which is the one piece of this module whose
            // size is decided by a `TextEdit` sharing a line with a chip. A
            // field that overran the panel would look exactly like a field that
            // fitted, in every assertion anybody could write about it.
            ("4-naming", false, false, true, false, false),
            // A drag in flight, which is the only way anybody looks at the drop
            // ring. `the_drop_ring_covers_no_colour_and_reaches_no_neighbour`
            // pins the geometry and can say nothing about whether the mark
            // reads as "the colour lands here" beside the solid accent outline
            // that means "this is the colour in hand" — which is the whole
            // argument for it being dashed and square.
            ("5-dragging", false, false, false, true, false),
            // The paste pane, which is a multiline field, a readout whose
            // height depends on what was found, and a button strip, stacked
            // above a list whose own height it just took a slice out of.
            // Nothing anybody can assert about a `TextEdit` says whether that
            // stack still fits in the modal.
            ("6-pasting", true, false, false, false, true),
        ] {
            let mut ed = Editor::default();
            ed.layout = Layout::default();
            ed.color = umber_core::Color::from_srgb_u8(143, 88, 42, 255);
            let library = if empty {
                let dir = std::env::temp_dir().join("umber-palette-shot-empty");
                let _ = std::fs::remove_dir_all(&dir);
                PaletteLibrary::load_from(&dir)
            } else {
                staged_library(name)
            };
            let mut seed = State {
                store: Store::Ready(Arc::new(library)),
                selected: None,
                library_open: open,
                renaming: None,
                naming: None,
                pasting: None,
                confirming: None,
            };
            seed.settle_selection();
            // The panel shots want the palette with something in it, which is
            // not the one that sorts first.
            if !empty && let Some(library) = seed.library() {
                seed.selected = library
                    .palettes()
                    .iter()
                    .find(|q| !q.is_empty())
                    .map(|q| q.id.clone());
            }
            if naming && let Some(palette) = seed.current() {
                seed.naming = Some(Naming {
                    palette: palette.id.clone(),
                    index: 2,
                    rgb: palette.swatches[2].rgb,
                    text: "Raw sienna".to_owned(),
                    // Never asked for in a shot: `request_focus` on a context
                    // that is redrawn from scratch every frame would fight the
                    // seeding above, and a caret is not what this is a picture
                    // of.
                    focus: false,
                });
            }
            if pasting {
                let mut typed = Pasting {
                    text: "https://coolors.co/10121c-2c1e31-6b2643\n#ac2847\n\
                           --warm-ochre: #cc772280;"
                        .to_owned(),
                    // Never asked for in a shot, for the reason the naming
                    // field's is not.
                    focus: false,
                    ..Pasting::default()
                };
                typed.reread();
                seed.pasting = Some(typed);
            }

            // Carrying the second colour, aiming at the seventh. Seeded through
            // `aiming_for_test` because `aim` needs the cells, and the cells do
            // not exist until the grid has been drawn.
            let carried = dragging.then(|| {
                swatchdrag::Drag::aiming_for_test(seed.selected.clone().unwrap_or_default(), 1, 6)
            });

            let palette = ed.palette();
            let field = if open {
                vec2(metrics::PALETTE_LIBRARY[0] + 80.0, 560.0)
            } else {
                vec2(metrics::PANEL, 300.0)
            };
            let rect = Rect::from_min_size(Pos2::ZERO, field);
            let image = stage.shoot(field, 2.0, &palette, palette.dock, |root| {
                // Re-seeded every frame: the state is read back out of egui's
                // memory, and the first frame of a fresh context would
                // otherwise go to the real user library.
                store(root.ctx(), seed.clone());
                if let Some(carried) = carried.clone() {
                    root.ctx()
                        .data_mut(|d| d.insert_temp(drag_id(), carried.clone()));
                }
                if open {
                    dialogs(root, &palette, &mut ed);
                } else {
                    let mut actions = crate::ui::UiActions::default();
                    crate::panels::panel(
                        root,
                        &palette,
                        &mut ed,
                        &mut actions,
                        PanelKind::Palette,
                        rect,
                    );
                }
            });
            docshot::write_png(&dir.join(format!("{name}.png")), &image).expect("write the png");
        }
        println!("wrote the shots to {}", dir.display());
    }

    /// A palette laid out in fours reads as fours where there is room, and a
    /// palette laid out in sixteen does not force sixteen slivers into a narrow
    /// panel. And whatever the answer, it is at least one — a zero would be a
    /// division by zero in `swatch_rect`.
    #[test]
    fn the_grid_honours_the_files_columns_only_where_they_fit() {
        let step = metrics::PALETTE_SWATCH + metrics::PALETTE_SWATCH_GAP;
        // The design's panel, less its padding: room for eight across.
        let panel = metrics::PANEL - 2.0 * metrics::PANEL_PAD as f32;
        assert_eq!(grid_columns(panel, 0), 8);
        assert_eq!(grid_columns(panel, 4), 4, "a palette of fours stays fours");
        assert_eq!(grid_columns(panel, 16), 8, "sixteen does not fit, so eight");
        // Exactly three swatches' worth of room is three columns, not two and a
        // half rounded either way.
        assert_eq!(grid_columns(step * 3.0 - metrics::PALETTE_SWATCH_GAP, 0), 3);
        for width in [0.0, 1.0, -50.0, metrics::PALETTE_SWATCH] {
            assert!(grid_columns(width, 0) >= 1, "width {width}");
            assert!(grid_columns(width, 16) >= 1, "width {width}");
        }
    }

    /// Two swatches must never share a pixel: the one drawn second would take
    /// every click on the overlap, so a colour would be unreachable — and the
    /// remove mark in its corner would remove the wrong one.
    #[test]
    fn the_swatches_never_overlap_and_wrap_where_they_should() {
        let origin = pos2(10.0, 20.0);
        for columns in 1..=8usize {
            let cells: Vec<Rect> = (0..17).map(|i| swatch_rect(origin, i, columns)).collect();
            for (i, a) in cells.iter().enumerate() {
                assert_eq!(a.width(), metrics::PALETTE_SWATCH);
                assert_eq!(a.height(), metrics::PALETTE_SWATCH);
                for b in &cells[i + 1..] {
                    assert!(!a.intersects(*b), "{columns} columns: {a:?} and {b:?}");
                }
            }
            // The row wraps exactly at the column count, and the first of each
            // row is back at the left.
            assert_eq!(
                cells[0].min,
                cells[columns].min - vec2(0.0, cells[0].height() + metrics::PALETTE_SWATCH_GAP)
            );
            assert_eq!(cells[columns].left(), origin.x);
        }
        // A zero column count would divide by zero; it is floored instead.
        assert_eq!(
            swatch_rect(origin, 3, 0).min.y,
            origin.y + 3.0 * (metrics::PALETTE_SWATCH + metrics::PALETTE_SWATCH_GAP)
        );
    }

    /// Both corner marks have to be wholly inside the swatch they act on, and
    /// they must not touch each other. One reaching over the neighbour would
    /// take its clicks, so pointing at one colour would delete the one before
    /// it; the two overlapping would put "name this" and "remove this" on the
    /// same pixels, and remove is the one that cannot be taken back. Both are
    /// only ever *drawn* while the pointer is inside their own swatch, so an
    /// overhang would also be a live target with nothing painted on it.
    #[test]
    fn the_corner_marks_stay_inside_their_own_swatch_and_off_each_other() {
        let origin = pos2(10.0, 20.0);
        for columns in 1..=8usize {
            for index in 0..17 {
                let cell = swatch_rect(origin, index, columns);
                let remove = remove_rect(cell);
                let name = name_rect(cell);
                for (what, mark) in [("remove", remove), ("name", name)] {
                    assert!(
                        cell.contains_rect(mark),
                        "{columns} columns, swatch {index}: the {what} mark \
                         {mark:?} escapes {cell:?}"
                    );
                }
                assert!(
                    !remove.intersects(name),
                    "{columns} columns, swatch {index}: {remove:?} and {name:?} share pixels"
                );
            }
        }
    }

    /// The drop ring is drawn in the gap *around* the cell it names, so it may
    /// not reach a neighbour: a mark saying "the colour lands here" over the
    /// colour beside the one it means is worse than no mark. And it must not
    /// cover the colour it names either — that is the whole reason it is a ring
    /// rather than the layer list's wash of the accent, since the fill would
    /// tint the swatch and this panel's one job is to show colours as they are.
    ///
    /// Measured against `drop_ring_rect`, which is what `drop_ring` actually
    /// draws on. Recomputing the expression here would leave this checking its
    /// own arithmetic: widening the real one to eat the neighbour would not
    /// fail a single line below.
    #[test]
    fn the_drop_ring_covers_no_colour_and_reaches_no_neighbour() {
        let origin = pos2(10.0, 20.0);
        for columns in 1..=8usize {
            let cells: Vec<Rect> = (0..17).map(|i| swatch_rect(origin, i, columns)).collect();
            for (index, cell) in cells.iter().enumerate() {
                let ring = drop_ring_rect(*cell);
                // Outside the colour on every side, so nothing is painted over
                // it. Stated as four strict inequalities rather than
                // `contains_rect`, which a ring drawn *inside* the cell — the
                // failure this is really about — would also satisfy.
                assert!(
                    ring.left() < cell.left()
                        && ring.top() < cell.top()
                        && ring.right() > cell.right()
                        && ring.bottom() > cell.bottom(),
                    "{columns} columns, swatch {index}: {ring:?} is not outside {cell:?}"
                );
                for (other, neighbour) in cells.iter().enumerate() {
                    if other != index {
                        assert!(
                            !ring.intersects(*neighbour),
                            "{columns} columns: swatch {index}'s ring reaches {other}"
                        );
                    }
                }
            }
        }
    }

    /// A press on either corner mark must not pick the colour up.
    ///
    /// The marks are inside the cell, so the plain "is the press inside a
    /// swatch" test accepts one — and egui calls a press a drag on **time**
    /// alone once `max_click_duration` has passed, with no movement at all. So
    /// holding Remove for six tenths of a second and letting go a cell over
    /// silently rearranged the palette instead of removing a colour, and wrote
    /// the file on the spot. There is no undo for a palette.
    #[test]
    fn a_press_on_a_corner_mark_does_not_pick_the_colour_up() {
        let origin = pos2(10.0, 20.0);
        let cells: Vec<swatchdrag::Cell> = (0..12)
            .map(|index| swatchdrag::Cell {
                index,
                rect: swatch_rect(origin, index, 4),
            })
            .collect();
        for cell in &cells {
            for mark in [remove_rect(cell.rect), name_rect(cell.rect)] {
                assert_eq!(
                    drag_origin(&cells, mark.center()),
                    None,
                    "swatch {}: a press on {mark:?} started a drag",
                    cell.index
                );
            }
            // The colour itself still picks up, or the marks would have made
            // the whole top of every swatch dead to a drag.
            assert_eq!(drag_origin(&cells, cell.rect.center()), Some(cell.index));
        }
        // And a press on nothing is still nothing.
        assert_eq!(drag_origin(&cells, pos2(0.0, 0.0)), None);
    }

    /// The naming field's own guard, which its doc calls structural. It is a
    /// pure function of the state so that this can exist at all: written inline
    /// in the drawing code it would have been a rule nothing could check, which
    /// is the division CLAUDE.md draws everywhere else.
    #[test]
    fn a_naming_field_whose_colour_moved_under_it_names_nothing() {
        let dir = std::env::temp_dir().join(format!("umber-palette-naming-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut library = PaletteLibrary::load_from(&dir);
        let id = library.create("Ochres").expect("made");
        let mut palette = library.get(&id).expect("there").clone();
        for rgb in [[1, 2, 3], [4, 5, 6], [7, 8, 9]] {
            palette.add(umber_core::Swatch::new(rgb));
        }
        library.save(palette).expect("saved");

        let mut state = State {
            store: Store::Ready(Arc::new(library)),
            selected: Some(id.clone()),
            library_open: false,
            renaming: None,
            naming: None,
            pasting: None,
            confirming: None,
        };
        state.naming = naming_for(&state, 1);
        let naming = state.naming.clone().expect("a field on the middle colour");
        assert_eq!(naming.rgb, [4, 5, 6]);
        assert!(naming_is_live(&state, &naming));

        // The colour it names taken out from under it: position 1 now holds
        // what position 2 held, and a field that still believed its index would
        // write the typed name onto a colour nobody was naming.
        let mut moved = state.current().expect("there").clone();
        assert!(moved.remove(0).is_some());
        if let Store::Ready(library) = &mut state.store {
            Arc::make_mut(library).save(moved).expect("saved");
        }
        assert!(
            !naming_is_live(&state, &naming),
            "the field survived the colour under it changing"
        );

        // A field belonging to another palette is not this one's either, and
        // an index past the end names nothing at all.
        let elsewhere = Naming {
            palette: "somewhere-else".to_owned(),
            ..naming.clone()
        };
        assert!(!naming_is_live(&state, &elsewhere));
        let past = Naming {
            index: 99,
            ..naming
        };
        assert!(!naming_is_live(&state, &past));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A disabled control says why, and the three reasons are not
    /// interchangeable: "there is nowhere to keep palettes on this system" is
    /// not "make a palette first", and neither is "this one is full". A tooltip
    /// naming the wrong one is the control lying about itself, which is the
    /// failure the whole disabled-with-a-reason arrangement exists to avoid.
    #[test]
    fn a_dead_adding_mark_gives_the_reason_it_is_dead() {
        let dir =
            std::env::temp_dir().join(format!("umber-palette-reasons-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut library = PaletteLibrary::load_from(&dir);
        let id = library.create("Ochres").expect("made");

        let mut state = State {
            store: Store::Broken("this system has no user data directory".to_owned()),
            selected: None,
            library_open: false,
            renaming: None,
            naming: None,
            pasting: None,
            confirming: None,
        };
        // Nowhere to write beats everything, and the wording is the library's
        // own rather than one invented here.
        assert_eq!(no_room_because(&state, 1), state.why_not());
        assert_eq!(no_room_because(&state, 3), state.why_not());

        // A library with nothing selected: make one first, whatever the count.
        state.store = Store::Ready(Arc::new(library.clone()));
        state.selected = None;
        for count in [1, 3] {
            assert!(
                no_room_because(&state, count).starts_with("Make a palette first"),
                "{count}: {}",
                no_room_because(&state, count)
            );
        }

        // A palette that is full: the single mark says the palette is full and
        // the harmony's says how many would not fit, because "already holds
        // 4096 colours" does not tell somebody adding four whether one of them
        // would have gone in.
        let mut full = library.get(&id).expect("there").clone();
        for _ in 0..palette::MAX_SWATCHES {
            assert!(full.add(umber_core::Swatch::new([0, 0, 0])));
        }
        library.save(full).expect("saved");
        state.store = Store::Ready(Arc::new(library));
        state.selected = Some(id);
        assert!(no_room_because(&state, 1).contains(&palette::MAX_SWATCHES.to_string()));
        assert_eq!(
            no_room_because(&state, 3),
            "There is not room for 3 more colours in this palette"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
