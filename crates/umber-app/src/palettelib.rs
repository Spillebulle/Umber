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

#[derive(Clone)]
struct State {
    store: Store,
    /// The palette in front, by id. See the module docs.
    selected: Option<String>,
    library_open: bool,
    renaming: Option<Renaming>,
    /// The colour whose name is being typed, if any.
    naming: Option<Naming>,
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
        controls::note(ui, p, state.why_not());
        store(ui.ctx(), state);
        return;
    }

    palette_picker(ui, p, &mut state);
    ui.add_space(8.0);

    if state.current().is_none() {
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
                palette.remove(index);
            });
        }
        Some(Act::Move { from, to }) => {
            state.naming = None;
            // The one write a drag makes, at the release. See the module docs:
            // a save reaches the disk immediately, so aiming must not write.
            edit_current(&mut state, ed, "Could not save the palette", |palette| {
                palette.move_swatch(from, to);
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
    let live = state.naming.as_ref().is_some_and(|naming| {
        state.selected.as_deref() == Some(naming.palette.as_str())
            && state
                .current()
                .and_then(|q| q.swatches.get(naming.index))
                .is_some_and(|swatch| swatch.rgb == naming.rgb)
    });
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
        shown.on_hover_text(format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2]));

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
                palette.name_swatch(index, &name);
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

/// The dash rhythm the dock's drop indicator and the layer list's drop slot are
/// both drawn with, so the three read as one mark rather than three.
const DROP_DASH: f32 = 5.0;
const DROP_GAP: f32 = 4.0;

/// Where a dragged colour would land: a dashed accent ring in the gap **around**
/// the cell it would take.
///
/// Three decisions, and each is against something the layer list does.
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
/// **Square-cornered**, which is where this departs from the rounded house
/// mark. At a 1.5 px offset a rounded ring traces the swatch's own outline and
/// reads as a thicker copy of it; square corners make it legibly a frame
/// *round* the colour rather than a second edge *on* it. It is also why this is
/// five points handed to egui's own `dashed_line` rather than a rounded outline
/// restated — there is no arc here to get wrong.
fn drop_ring(painter: &egui::Painter, p: &Palette, cell: Rect) {
    let ring = cell.expand(metrics::PALETTE_SWATCH_GAP * 0.5 - 0.5);
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
        DROP_DASH,
        DROP_GAP,
    ));
}

/// Where the colour being carried is kept between frames.
///
/// In egui's temporary store rather than on `Editor`, for the reason the layer
/// drag's is: it belongs to this grid, not to a document.
fn drag_id() -> Id {
    Id::new("palette-swatch-drag")
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
        if (pointer_inside && !dragging) || named == Some(index) {
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
        && let Some(index) = origin.and_then(|at| swatchdrag::cell_pressed(&cells, at))
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
         it, or bring one in. Umber reads and writes GIMP's .gpl, which GIMP, \
         Krita, Inkscape and Aseprite all read.",
    );
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if controls::text_button(ui, p, "New palette", true, true).clicked() {
            new_palette(state, ed);
        }
        if controls::text_button(ui, p, "Import…", false, true).clicked() {
            import(state, ed);
        }
    });
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Change the palette in front, and write it out.
///
/// One door, so every edit reads the palette, changes it and saves it in the
/// same order — and so the write's failure is reported once rather than at four
/// call sites.
fn edit_current(
    state: &mut State,
    ed: &mut Editor,
    what: &str,
    change: impl FnOnce(&mut ColourPalette),
) {
    let Some(mut palette) = state.current().cloned() else {
        return;
    };
    change(&mut palette);
    write(state, ed, what, |library| library.save(palette));
}

fn add_current_colour(state: &mut State, ed: &mut Editor) {
    let swatch = Swatch::of(ed.color);
    edit_current(state, ed, "Could not save the palette", |palette| {
        palette.add(swatch);
    });
}

/// Put the harmony of the colour in hand into the palette in front.
///
/// **The whole set or none of it** — `Palette::add_all` is the rule and the
/// argument. The mark above is disabled where it will not fit, so this second
/// gate is for the palette having changed between the frame the mark was drawn
/// and the click; without it `edit_current` would write the file to say nothing
/// happened.
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
    if !state
        .current()
        .is_some_and(|palette| palette.room_for(swatches.len()))
    {
        return;
    }
    edit_current(state, ed, "Could not save the palette", |palette| {
        palette.add_all(&swatches);
    });
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

/// Bring in one or more `.gpl` files.
///
/// Several at once because a palette collection *is* a folder of them, which is
/// the same reason the brush importer takes several files.
fn import(state: &mut State, ed: &mut Editor) {
    if !state.writable() {
        return;
    }
    let Some(paths) = rfd::FileDialog::new()
        .set_title("Import palettes")
        .add_filter("GIMP palette", &[GPL_EXTENSION])
        // Deliberately present and deliberately last: picking the wrong kind of
        // file gives a sentence naming it and the reason, which is a better
        // answer than a picker that refuses to show the file at all.
        .add_filter("All files", &["*"])
        .pick_files()
    else {
        return;
    };

    let mut added = None;
    let mut lines = Vec::new();
    for path in &paths {
        let Store::Ready(library) = &mut state.store else {
            return;
        };
        match Arc::make_mut(library).import(path) {
            Ok((id, skipped)) => {
                if skipped > 0 {
                    // An import that loses something says so. Every reader in
                    // the wild skips a line it cannot parse, so this is a note
                    // rather than a refusal — but a silent one would be the
                    // artist wondering where three colours went.
                    lines.push(format!(
                        "{}: {skipped} line{} were not colours and were skipped.",
                        file_label(path),
                        if skipped == 1 { "" } else { "s" }
                    ));
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
    let h = full_height.min(available.y - 220.0).max(120.0);

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
            controls::note(
                ui,
                p,
                "Every palette is one .gpl file in a folder of its own. That is \
                 the format GIMP, Krita, Inkscape and Aseprite all read, so \
                 importing is bringing a file in and exporting is taking one \
                 out.",
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
                        "Bring a .gpl palette into your library".to_owned()
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
        let dir = std::env::temp_dir().join("umber-palette-settle");
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

        for (name, open, empty, naming, dragging) in [
            ("1-panel", false, false, false, false),
            ("2-panel-empty", false, true, false, false),
            ("3-library", true, false, false, false),
            // The naming field, which is the one piece of this module whose
            // size is decided by a `TextEdit` sharing a line with a chip. A
            // field that overran the panel would look exactly like a field that
            // fitted, in every assertion anybody could write about it.
            ("4-naming", false, false, true, false),
            // A drag in flight, which is the only way anybody looks at the drop
            // ring. `the_drop_ring_covers_no_colour_and_reaches_no_neighbour`
            // pins the geometry and can say nothing about whether the mark
            // reads as "the colour lands here" beside the solid accent outline
            // that means "this is the colour in hand" — which is the whole
            // argument for it being dashed and square.
            ("5-dragging", false, false, false, true),
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
        println!("wrote 5 shots to {}", dir.display());
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
    /// colour beside the one it means is worse than no mark. Half the gap each
    /// side, less half a pixel for the stroke, is what leaves it clear.
    ///
    /// And it must not cover the colour either. That is the whole reason it is
    /// a ring rather than the layer list's wash of the accent — the fill would
    /// tint the swatch, and this panel's one job is to show colours as they
    /// are.
    #[test]
    fn the_drop_ring_covers_no_colour_and_reaches_no_neighbour() {
        let origin = pos2(10.0, 20.0);
        for columns in 1..=8usize {
            let cells: Vec<Rect> = (0..17).map(|i| swatch_rect(origin, i, columns)).collect();
            for (index, cell) in cells.iter().enumerate() {
                let ring = cell.expand(metrics::PALETTE_SWATCH_GAP * 0.5 - 0.5);
                assert!(
                    ring.contains_rect(*cell),
                    "{columns} columns, swatch {index}: the ring is inside the colour"
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
}
