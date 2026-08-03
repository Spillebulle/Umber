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
//! grey being taken. The remove mark inside a swatch's corner is *allocated*
//! every frame and *painted* only while the pointer is inside the swatch, which
//! is the rule a revealed control has to follow here: egui stops its hover
//! search at the topmost interactive widget, so a mark that only existed while
//! its parent reported hovered would flicker once a frame. Testing the
//! swatch's own rectangle is geometry, and geometry does not oscillate.

use std::path::Path;
use std::sync::Arc;

use egui::{Color32, Frame, Id, Layout, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use umber_core::palette::{self, GPL_EXTENSION, Palette as Swatches, PaletteError, PaletteLibrary};

use crate::controls;
use crate::editor::Editor;
use crate::icons::Icon;
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

#[derive(Clone)]
struct State {
    store: Store,
    /// The palette in front, by id. See the module docs.
    selected: Option<String>,
    library_open: bool,
    renaming: Option<Renaming>,
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
    fn current(&self) -> Option<&Swatches> {
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

/// The two marks in the Palette panel's header: save the colour in hand, and
/// open the library.
pub fn header_controls(ui: &mut Ui, p: &Palette, ed: &mut Editor) {
    let mut state = load(ui.ctx(), ed);

    // Right-to-left: added first lands furthest right, next to the close mark,
    // which is where the Brushes panel draws its own save.
    let room = state.current().is_some_and(Swatches::has_room);
    let can_add = state.writable() && room;
    let tip = if can_add {
        "Add the colour in hand to this palette".to_owned()
    } else if !state.writable() {
        state.why_not().to_owned()
    } else if state.current().is_none() {
        "Make a palette first — the grid mark opens the library".to_owned()
    } else {
        format!(
            "This palette already holds {} colours",
            palette::MAX_SWATCHES
        )
    };
    if icon_button(ui, p, Icon::Plus, can_add, &tip) {
        add_current_colour(&mut state, ed);
    }
    if icon_button(
        ui,
        p,
        Icon::Grid,
        true,
        "Palettes — make, import and manage them",
    ) {
        state.library_open = true;
    }

    store(ui.ctx(), state);
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

    match swatch_grid(ui, p, ed, &state) {
        Some(Act::Take(index)) => {
            if let Some(swatch) = state.current().and_then(|s| s.swatches.get(index)) {
                let colour = swatch.colour();
                ed.set_color(colour);
            }
        }
        Some(Act::Remove(index)) => {
            edit_current(&mut state, ed, "Could not save the palette", |palette| {
                palette.remove(index);
            });
        }
        None => {}
    }

    store(ui.ctx(), state);
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

/// The remove mark inside a swatch's top-right corner.
const REMOVE_MARK: f32 = 11.0;

/// Where that mark sits, given the swatch it belongs to.
///
/// Its own function so the containment can be checked without a `Ui`: a mark
/// that reached outside its swatch would sit over the neighbour and take its
/// clicks, so pointing at one colour would remove the one before it.
fn remove_rect(cell: Rect) -> Rect {
    Rect::from_min_size(
        pos2(cell.right() - REMOVE_MARK - 1.0, cell.top() + 1.0),
        egui::Vec2::splat(REMOVE_MARK),
    )
}

fn swatch_grid(ui: &mut Ui, p: &Palette, ed: &Editor, state: &State) -> Option<Act> {
    let library = state.library()?.clone();
    let palette = library.get(state.selected.as_deref()?)?;

    if palette.is_empty() {
        controls::note(
            ui,
            p,
            "Nothing in this palette yet. The plus above adds the colour you \
             are painting with.",
        );
        return None;
    }

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
        let response = ui.interact(cell, ui.id().with(("swatch", index)), Sense::click());
        // Geometry, not `response.hovered()`. The remove mark below is an
        // interactive widget on top of this one, and egui stops its hover search
        // at the topmost — so a mark drawn only while the swatch reported
        // hovered would blink out the moment the pointer reached it, and back in
        // the frame after. See the module docs.
        let pointer_inside = ui
            .ctx()
            .pointer_hover_pos()
            .is_some_and(|at| cell.contains(at));

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

        // Allocated whether or not it is drawn, so its id and its rectangle do
        // not appear and disappear between frames.
        let mark = remove_rect(cell);
        let remove = ui.interact(mark, ui.id().with(("swatch-remove", index)), Sense::click());
        if pointer_inside {
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

        let name = if swatch.name.trim().is_empty() {
            swatch.hex()
        } else {
            format!("{} — {}", swatch.name, swatch.hex())
        };
        if pointer_inside && remove.on_hover_text("Remove this colour").clicked() {
            act = Some(Act::Remove(index));
        } else if response.on_hover_text(name).clicked() {
            act = Some(Act::Take(index));
        }
    }
    act
}

/// What the panel shows with no palette to show.
fn empty_library(ui: &mut Ui, p: &Palette, ed: &mut Editor, state: &mut State) {
    controls::note(
        ui,
        p,
        "No palettes yet. Make one and it fills with the colours you save into \
         it, or bring one in — Umber reads and writes GIMP's .gpl, which is \
         what every other painting application uses.",
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
    change: impl FnOnce(&mut Swatches),
) {
    let Some(mut palette) = state.current().cloned() else {
        return;
    };
    change(&mut palette);
    write(state, ed, what, |library| library.save(palette));
}

fn add_current_colour(state: &mut State, ed: &mut Editor) {
    let swatch = umber_core::Swatch::of(ed.color);
    edit_current(state, ed, "Could not save the palette", |palette| {
        palette.add(swatch);
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
        .set_file_name(format!("{}.{GPL_EXTENSION}", palette.name))
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
    let mut state = load(root.ctx(), ed);
    if !state.library_open {
        store(root.ctx(), state);
        return;
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
            let [w, h] = metrics::PALETTE_LIBRARY;
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
                "Every palette is one .gpl file in a folder of its own, which \
                 is the format GIMP, Krita, Inkscape and Aseprite all read — so \
                 importing is bringing a file in and exporting is taking one \
                 out.",
            );
            ui.add_space(10.0);

            ui.horizontal(|ui| {
                let writable = state.writable();
                if controls::text_button(ui, p, "New palette", true, writable)
                    .on_hover_text(if writable { "" } else { state.why_not() })
                    .clicked()
                {
                    new_palette(&mut state, ed);
                }
                if controls::text_button(ui, p, "Import…", false, writable).clicked() {
                    import(&mut state, ed);
                }
                if let Some(dir) = state.library().map(|library| library.dir().to_path_buf()) {
                    ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                        folder_link(ui, p, &dir);
                    });
                }
            });
            ui.add_space(8.0);

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
    let mut request = None;
    for palette in library.palettes() {
        if let Some(asked) = library_row(ui, p, state, palette) {
            request = Some((palette.id.clone(), asked));
        }
        ui.add_space(6.0);
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
        Some((id, Request::Rename(name))) => {
            state.renaming = None;
            write(state, ed, "Could not rename the palette", |library| {
                library.rename(&id, &name)
            });
        }
        Some((_, Request::CancelRename)) => state.renaming = None,
        Some((id, Request::Export)) => export(state, ed, &id),
        Some((id, Request::Confirm)) => state.confirming = Some(id),
        Some((id, Request::Delete)) => {
            state.confirming = None;
            write(state, ed, "Could not delete the palette", |library| {
                library.remove(&id)
            });
        }
        None => {}
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

fn library_row(ui: &mut Ui, p: &Palette, state: &mut State, palette: &Swatches) -> Option<Request> {
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
                            // elsewhere — keeps what was typed. `rename` is
                            // refused an empty name by the model, so a field
                            // cleared and blurred is not a nameless palette.
                            request = Some(if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                Request::CancelRename
                            } else {
                                Request::Rename(rename.text.clone())
                            });
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
                    let label = if confirming { "Really?" } else { "Delete" };
                    if controls::text_button(ui, p, label, confirming, true)
                        .on_hover_text(if confirming {
                            "This cannot be undone — the history covers painting only"
                        } else {
                            "Delete this palette"
                        })
                        .clicked()
                    {
                        request = Some(if confirming {
                            Request::Delete
                        } else {
                            Request::Confirm
                        });
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
                    if controls::text_button(ui, p, "Rename", false, true).clicked() {
                        request = Some(Request::StartRename);
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
fn colour_strip(ui: &mut Ui, p: &Palette, palette: &Swatches) {
    let width = ui.available_width().max(1.0);
    let (rect, _) = ui.allocate_exact_size(vec2(width, STRIP_HEIGHT), Sense::hover());
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
        use crate::theme::Palette as Theme;
        use egui::{Pos2, Rect, vec2};

        let Some(mut stage) = docshot::Stage::new() else {
            eprintln!("no GPU adapter: nothing to draw into. Skipped.");
            return;
        };
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/palette-module");
        std::fs::create_dir_all(&dir).expect("create the preview directory");

        for (name, open, empty) in [
            ("1-panel", false, false),
            ("2-panel-empty", false, true),
            ("3-library", true, false),
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

            let palette = Theme::with_accent(ed.ui.theme, ed.ui.accent);
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
        println!("wrote 3 shots to {}", dir.display());
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

    /// The remove mark has to be wholly inside the swatch it removes. One that
    /// reached over the neighbour would take its clicks, so pointing at one
    /// colour would delete the one before it — and it is only ever *drawn* while
    /// the pointer is inside its own swatch, so an overhang would be a live
    /// target with nothing painted on it.
    #[test]
    fn the_remove_mark_stays_inside_its_own_swatch() {
        let origin = pos2(10.0, 20.0);
        for columns in 1..=8usize {
            for index in 0..17 {
                let cell = swatch_rect(origin, index, columns);
                let mark = remove_rect(cell);
                assert!(
                    cell.contains_rect(mark),
                    "{columns} columns, swatch {index}: {mark:?} escapes {cell:?}"
                );
            }
        }
    }
}
