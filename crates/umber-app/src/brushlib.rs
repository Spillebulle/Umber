//! The brush library, in front of the user.
//!
//! `umber-core` already held all of this: 201 shipped presets with their
//! attribution ([`preset::builtin`]), a user library that writes itself to disk
//! on every change ([`UserLibrary`]), and importers for MyPaint, GIMP, Krita,
//! Photoshop and Umber's own `.ron`. None of it was reachable from the
//! interface, which listed the presets and offered nothing else. This module is
//! the reach.
//!
//! Five things here are worth knowing before changing them:
//!
//! - **`Editor::presets` is the merged list.** `Editor::apply_preset` selects
//!   by *index* into it, so a saved brush has to live in that vector to be
//!   selectable at all. [`resync`] rebuilds it as "everything shipped, then
//!   everything saved", and re-finds the selection by id — an index does not
//!   survive a delete.
//! - **Nothing allocates per frame on the drawing path.** The grouping, and the
//!   credit line each row shows, are built once per change to the library;
//!   searching walks borrowed data and folds case in place; the rows skip
//!   painting when they are scrolled out of view. At 239 presets the naive
//!   version of any of those is visible in a frame time.
//! - **State lives in egui's temporary store**, keyed by an `Id`, exactly the
//!   way `settings.rs` keeps what its shortcut table is in the middle of. It is
//!   the interaction state of a dialog, not state of the document.
//! - **Key dispatch happens at the winit level, before egui sees a keystroke**,
//!   so typing "brush" into the search box would otherwise select the brush,
//!   then the eraser, on the way past. Nothing here has to do anything about
//!   it: `ui::draw` suspends dispatch for whatever field holds the keyboard,
//!   for the whole interface at once. See [`crate::shortcuts::set_typing`].
//! - **[`resync`] carries the bitmap tips across too.** `BrushPreset::tip` is
//!   the *name* of a mask in the user's library, and the drawing path has no
//!   business reaching into a library; `Editor::tips` is where
//!   `Editor::apply_preset` resolves the name, so a preset saved with a stamp
//!   is only a stamp brush once this has run.
//!
//! The design draws a Brushes panel of five presets: a header with a `＋`, a
//! column of rows, and a link to the brush editor. All three are here. The
//! search field, the collection picker and the browser are not in the design,
//! because a column that works for five brushes does not work for 201 — see the
//! README.

use crate::controls;
use crate::editor::Editor;
use crate::icons::{self, Icon};
use crate::theme::{Palette, metrics, text};
use crate::ui::icon_button;
use crate::widgets::{self, BrushRow};
use egui::{
    Align, Align2, FontId, Frame, Id, Layout, Margin, Rect, Response, RichText, Sense, Stroke,
    TextEdit, Ui, UiBuilder, pos2, vec2,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use umber_core::preset::{self, BrushPreset, PresetError, UserLibrary};
use umber_core::style;

/// Width kept clear at the right of a browser row for its two controls.
const ROW_CONTROLS: f32 = 46.0;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// The user's own library, or the reason there isn't one.
///
/// A failure to *read* it is not a reason to carry on with an empty one: the
/// next save would write over whatever is actually in the file. So a broken
/// library disables everything that writes and says why, rather than quietly
/// starting the collection again.
#[derive(Clone)]
enum Store {
    /// An `Arc` because egui's temporary store hands back a *clone* of the
    /// whole state every frame, and copying the user's brush collection sixty
    /// times a second to draw a list is not a cost worth paying.
    Ready(Arc<UserLibrary>),
    Broken(String),
}

/// Which collection the lists are showing.
#[derive(Clone, PartialEq, Eq)]
enum Scope {
    All,
    Category(String),
}

/// Something that just happened, or just failed.
///
/// Failures carry [`PresetError`]'s own wording: it writes finished sentences
/// that name the file, which is more than this module knows.
#[derive(Clone)]
struct Notice {
    text: String,
    bad: bool,
}

impl Notice {
    fn good(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            bad: false,
        }
    }

    fn bad(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            bad: true,
        }
    }
}

/// Where the "name this brush" field was armed from, so arming it in the panel
/// cannot leave a second copy of it open in the brush editor.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SaveSite {
    Panel,
    Editor,
}

/// A field that has to take the keyboard when it appears.
///
/// `focus` is consumed on the first frame; asking for focus every frame instead
/// would take it straight back off anything else the user clicked.
#[derive(Clone)]
struct Field {
    text: String,
    focus: bool,
}

impl Field {
    fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            focus: true,
        }
    }
}

#[derive(Clone)]
struct State {
    store: Store,
    index: Arc<Index>,
    query: String,
    scope: Scope,
    browser_open: bool,
    saving: Option<(SaveSite, Field)>,
    /// The id of the user brush being renamed, and the name so far.
    renaming: Option<(String, Field)>,
    /// The id of the user brush whose Delete has been pressed once. Deleting a
    /// brush cannot be undone — the history covers painting only — so it asks.
    confirming: Option<String>,
    notice: Option<Notice>,
}

impl State {
    fn writable(&self) -> bool {
        matches!(self.store, Store::Ready(_))
    }

    /// The tooltip for a control that writes, when there is nothing to write
    /// to. Never invented wording: it is what the library itself reported.
    fn why_not(&self) -> &str {
        match &self.store {
            Store::Ready(_) => "",
            Store::Broken(why) => why,
        }
    }

    fn dir(&self) -> Option<&Path> {
        match &self.store {
            Store::Ready(library) => Some(library.dir()),
            Store::Broken(_) => None,
        }
    }
}

fn state_id() -> Id {
    Id::new("brush-library")
}

/// Read the state back, reading the library off disk on the first frame.
fn load(ctx: &egui::Context, ed: &mut Editor) -> State {
    if let Some(mut state) = ctx.data(|d| d.get_temp::<State>(state_id())) {
        // The one cheap guard that catches every way the merged list can have
        // moved under us: a save, a delete and an import all change its length.
        // A rename changes only a name, and the index holds positions and
        // collection names rather than names.
        if state.index.total != ed.presets.len() {
            state.index = Arc::new(Index::build(&ed.presets));
        }
        return state;
    }

    let mut notice = None;
    let store = match UserLibrary::load() {
        Ok(library) => {
            resync(ed, &library);
            // Both of these are once-per-launch and worth a sentence. A library
            // that has just moved, and a mask that would not open, are things
            // the user can act on — and the second one means a brush they saved
            // is quietly painting round.
            if !library.warnings().is_empty() {
                notice = Some(Notice::bad(format!(
                    "Some brush tips could not be read, so those brushes paint round: {}",
                    library.warnings().join("; ")
                )));
            } else if library.migrated() {
                notice = Some(Notice::good(format!(
                    "Your brushes have moved into {}, so they can carry bitmap tips. \
                     The old brushes.ron was left where it was.",
                    library.dir().display()
                )));
            }
            Store::Ready(Arc::new(library))
        }
        Err(e) => Store::Broken(e.to_string()),
    };
    State {
        index: Arc::new(Index::build(&ed.presets)),
        store,
        query: String::new(),
        scope: Scope::All,
        browser_open: false,
        saving: None,
        renaming: None,
        confirming: None,
        notice,
    }
}

fn store(ctx: &egui::Context, state: State) {
    ctx.data_mut(|d| d.insert_temp(state_id(), state));
}

// ---------------------------------------------------------------------------
// The index
// ---------------------------------------------------------------------------

/// Where every preset sits in the grouping, as positions in `Editor::presets`.
///
/// Positions rather than clones: the lists redraw every frame, and the presets
/// they draw are already in the editor.
struct Index {
    groups: Vec<Group>,
    /// The credit line for each preset, parallel to `Editor::presets`.
    ///
    /// Formatted here rather than in the row, because the row runs 201 times a
    /// frame and this runs when the library changes.
    details: Vec<String>,
    /// `Editor::presets.len()` when this was built — the staleness check.
    total: usize,
    /// Everything from here on came out of the user's library.
    shipped: usize,
}

struct Group {
    name: String,
    members: Vec<usize>,
}

impl Index {
    fn build(presets: &[BrushPreset]) -> Self {
        let shipped = preset::builtin().len();
        let mut groups: Vec<Group> = Vec::new();
        for (i, preset) in presets.iter().enumerate() {
            let name = collection_of(preset);
            match groups.iter_mut().find(|g| g.name == name) {
                Some(group) => group.members.push(i),
                None => groups.push(Group {
                    name: name.to_owned(),
                    members: vec![i],
                }),
            }
        }
        // Yours first, then the styles in the order `umber_core::style`
        // declares them — roughly the order a painter works in, drawing media
        // through paint to the things done to paint already down. A library you
        // cannot add to is a reference; the brushes you made are the ones you
        // are reaching for, so they stay at the top.
        groups.sort_by(|a, b| {
            rank(a, shipped)
                .cmp(&rank(b, shipped))
                .then_with(|| a.name.cmp(&b.name))
        });
        Self {
            total: presets.len(),
            shipped,
            details: presets.iter().map(credit_line).collect(),
            groups,
        }
    }

    fn is_user(&self, index: usize) -> bool {
        index >= self.shipped
    }
}

fn collection_of(preset: &BrushPreset) -> &str {
    if preset.category.is_empty() {
        "Uncategorised"
    } else {
        &preset.category
    }
}

/// Sort key for a collection: yours first, then styles in their declared order,
/// then anything an imported library brought its own name for.
fn rank(group: &Group, shipped: usize) -> usize {
    if group.members.iter().all(|i| *i >= shipped) {
        return 0;
    }
    1 + style::order_of(&group.name)
}

/// Walk the presets in `scope` that match `query`, in display order.
///
/// An iterator over borrowed data rather than a filtered `Vec`: this runs on
/// every frame of both lists, and building 201 entries a frame to draw fifteen
/// visible rows is exactly the waste immediate mode invites.
fn visit<'a>(
    index: &'a Index,
    presets: &'a [BrushPreset],
    scope: &'a Scope,
    query: &'a str,
) -> impl Iterator<Item = usize> + 'a {
    index
        .groups
        .iter()
        .filter(move |group| match scope {
            Scope::All => true,
            Scope::Category(name) => group.name == *name,
        })
        .flat_map(|group| group.members.iter().copied())
        .filter(move |i| matches(&presets[*i], query))
}

/// Name or collection contains `query`, which the caller has already lowered.
fn matches(preset: &BrushPreset, query: &str) -> bool {
    query.is_empty()
        || contains_ignore_case(&preset.name, query)
        || contains_ignore_case(&preset.category, query)
}

/// Case-insensitive substring, without lowering a copy of the haystack.
///
/// `to_lowercase` on every name on every frame is 201 allocations to answer a
/// question about fifteen visible rows. The fold is ASCII-only, which suits the
/// data: the non-ASCII in the shipped library is inside author names ("Ramón
/// Miranda"), where the accented bytes are identical either way and match
/// regardless.
fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    let (h, n) = (haystack.as_bytes(), needle.as_bytes());
    // The empty needle is in everything — and `windows(0)` panics rather than
    // yielding nothing, so this is a guard and not just an early out.
    if n.is_empty() {
        return true;
    }
    n.len() <= h.len() && h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
}

/// `David Revoy · CC0-1.0`, or the collection when there is no credit.
///
/// Shown on the row itself in the browser rather than only in a tooltip: CC0
/// needs no attribution but recording the authorship is the whole point of
/// carrying it, and anything CC-BY has to be credited wherever it is offered.
fn credit_line(preset: &BrushPreset) -> String {
    match &preset.credit {
        Some(credit) => match (credit.author.is_empty(), credit.licence.is_empty()) {
            (false, false) => format!("{} · {}", credit.author, credit.licence),
            (false, true) => credit.author.clone(),
            (true, false) => credit.licence.clone(),
            (true, true) => preset.category.clone(),
        },
        // A brush the user made or imported carries no credit, and inventing
        // one would be worse than saying where it sits.
        None => preset.category.clone(),
    }
}

/// Whether a licence obliges the user to name the author.
///
/// Deliberately crude and deliberately cautious: anything that is not plainly a
/// public-domain dedication is treated as needing credit, because the failure
/// direction of the other guess is a licence breach.
fn requires_attribution(licence: &str) -> bool {
    let licence = licence.trim();
    !(licence.is_empty()
        || contains_ignore_case(licence, "cc0")
        || contains_ignore_case(licence, "public domain")
        || contains_ignore_case(licence, "unlicense"))
}

// ---------------------------------------------------------------------------
// Keeping the editor in step
// ---------------------------------------------------------------------------

/// Make `Editor::presets` hold everything shipped followed by everything saved.
///
/// `Editor::apply_preset` takes an index into that vector, so a saved brush
/// that is not in it cannot be selected at all. The selection is re-found by
/// id rather than kept, because deleting a brush moves every index after it,
/// and a stale index would silently select a different brush.
fn resync(ed: &mut Editor, library: &UserLibrary) {
    let selected = ed
        .active_preset
        .and_then(|i| ed.presets.get(i))
        .map(|preset| preset.id.clone());
    ed.presets.truncate(preset::builtin().len());
    ed.presets.extend(library.presets().iter().cloned());
    ed.active_preset = selected.and_then(|id| ed.presets.iter().position(|p| p.id == id));
    // The masks come across too, so the drawing path can resolve a preset's tip
    // without reaching into the library. Cloning the map is cloning a handful of
    // `Arc`s, not the bitmaps, and it happens when the library changes rather
    // than per frame.
    ed.tips = library.tips().clone();
}

/// Run a write against the user's library and put everything back in step.
///
/// Every [`UserLibrary`] write reaches the disk immediately, so this is also
/// where a failed write becomes something the user can read rather than a log
/// line nobody sees. `None` means it did not happen — either there is no
/// library to write to, or the write failed and the notice already says so.
fn write<T>(
    state: &mut State,
    ed: &mut Editor,
    op: impl FnOnce(&mut UserLibrary) -> Result<T, PresetError>,
) -> Option<T> {
    let Store::Ready(library) = &mut state.store else {
        return None;
    };
    match op(Arc::make_mut(library)) {
        Ok(value) => {
            // Re-borrowed rather than held: the mutable borrow above has to end
            // before the index can be replaced.
            let Store::Ready(library) = &state.store else {
                return None;
            };
            let library = Arc::clone(library);
            resync(ed, &library);
            state.index = Arc::new(Index::build(&ed.presets));
            Some(value)
        }
        Err(e) => {
            state.notice = Some(Notice::bad(e.to_string()));
            None
        }
    }
}

// ---------------------------------------------------------------------------
// The Brushes panel
// ---------------------------------------------------------------------------

/// The two marks in the Brushes panel's header: browse, and save.
///
/// The design puts a `＋` there. The second mark is this module's addition —
/// with 239 presets the panel is a shortlist rather than the library, and
/// something has to open the rest of it.
pub fn header_controls(ui: &mut Ui, p: &Palette, ed: &mut Editor) {
    let mut state = load(ui.ctx(), ed);
    let writable = state.writable();

    // Right-to-left: added first lands furthest right, next to the close mark,
    // which is where the design draws the `＋`.
    if icon_button(
        ui,
        p,
        Icon::Plus,
        writable,
        if writable {
            "Save the current brush to your library"
        } else {
            state.why_not()
        },
    ) {
        state.saving = Some((SaveSite::Panel, Field::new(suggested_name(ed))));
        state.notice = None;
    }
    if icon_button(ui, p, Icon::Grid, true, "Browse the whole brush library") {
        state.browser_open = true;
    }

    store(ui.ctx(), state);
}

/// The Brushes panel body: search, collection, the shortlist, and the links.
pub fn panel(ui: &mut Ui, p: &Palette, ed: &mut Editor) {
    let mut state = load(ui.ctx(), ed);

    if let Store::Broken(why) = &state.store {
        let why = why.clone();
        notice_bar(ui, p, &Notice::bad(why), false);
        ui.add_space(6.0);
    }
    // The browser owns the notice while it is up, so the same sentence is never
    // reported in two places at once.
    if !state.browser_open
        && let Some(notice) = state.notice.clone()
    {
        if notice_bar(ui, p, &notice, true) {
            state.notice = None;
        }
        ui.add_space(6.0);
    }

    // The design's `＋` is in the header; the field it arms has to appear
    // somewhere, and the top of the panel is where the new brush will land.
    save_field(ui, p, ed, &mut state, SaveSite::Panel);

    controls::search_field(ui, p, &mut state.query, "Search brushes");
    ui.add_space(5.0);
    collection_row(ui, p, ed, &mut state);
    ui.add_space(3.0);

    let out = list(ui, p, ed, &state, metrics::BRUSH_ROW, false, None);
    if let Some(index) = out.picked {
        ed.apply_preset(index);
    }

    ui.add_space(7.0);
    panel_links(ui, p, ed, &mut state);

    store(ui.ctx(), state);
}

/// The design's `✎ Edit "<name>"…` link, plus the way into an import.
fn panel_links(ui: &mut Ui, p: &Palette, ed: &mut Editor, state: &mut State) {
    // The design writes the brush's own name into the link. It drew five
    // presets with short names; the shipped library has "Coarse Bulk 1", so the
    // label is elided rather than allowed to push the row past the panel edge.
    let label = format!("Edit \"{}\"…", active_name(ed));
    if link(ui, p, Icon::Pencil, &label, true)
        .on_hover_text("Open the brush editor")
        .clicked()
    {
        ed.ui.brush_editor_open = true;
    }

    let writable = state.writable();
    if link(ui, p, Icon::Import, "Import brushes…", writable)
        .on_hover_text(if writable {
            "Read MyPaint, GIMP, Krita or Photoshop brushes, or an Umber .ron library"
        } else {
            state.why_not()
        })
        .clicked()
    {
        import(state, ed);
    }
}

/// The collection picker: what is being shown, how many of them, and a menu of
/// the alternatives.
///
/// Not in the design, which had one collection of five. Seven shipped
/// collections plus the user's own need a way to say which one.
fn collection_row(ui: &mut Ui, p: &Palette, ed: &Editor, state: &mut State) {
    let query = state.query.trim().to_lowercase();
    let count = visit(&state.index, &ed.presets, &state.scope, &query).count();
    let label = match &state.scope {
        Scope::All => "All collections",
        Scope::Category(name) => name.as_str(),
    };

    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::click());
    let ink = if response.hovered() {
        p.text_strong
    } else {
        p.text_dim
    };
    let painter = ui.painter();
    let font = FontId::proportional(text::TINY);
    painter.text(
        rect.left_center(),
        Align2::LEFT_CENTER,
        widgets::elide(painter, label, text::TINY, rect.width() - 52.0),
        font,
        ink,
    );
    painter.text(
        rect.right_center(),
        Align2::RIGHT_CENTER,
        count.to_string(),
        FontId::monospace(9.5),
        p.text_dim,
    );
    icons::draw(
        painter,
        Rect::from_min_size(pos2(rect.right() - 36.0, rect.top()), vec2(12.0, 20.0)),
        Icon::ChevronDown,
        ink,
    );

    let mut chosen = None;
    egui::Popup::menu(&response).show(|ui| {
        ui.set_min_width(190.0);
        let all = format!("All collections ({})", state.index.total);
        if ui
            .selectable_label(state.scope == Scope::All, all)
            .clicked()
        {
            chosen = Some(Scope::All);
            ui.close();
        }
        ui.separator();
        for group in &state.index.groups {
            let scope = Scope::Category(group.name.clone());
            let label = format!("{} ({})", group.name, group.members.len());
            if ui.selectable_label(state.scope == scope, label).clicked() {
                chosen = Some(scope);
                ui.close();
            }
        }
    });
    if let Some(scope) = chosen {
        state.scope = scope;
    }
}

// ---------------------------------------------------------------------------
// The list, shared by the panel and the browser
// ---------------------------------------------------------------------------

/// What a row asked for. At most one per frame: acting on any of them rewrites
/// the list the loop is walking.
enum Request {
    /// Start renaming this brush.
    Rename(String),
    /// Arm the delete on this brush.
    Confirm(String),
    /// Back out of whatever is in progress.
    Cancel,
    /// Rename it to this.
    Commit(String, String),
    Delete(String),
}

#[derive(Default)]
struct ListOut {
    picked: Option<usize>,
    /// Where the row named by `editing` landed, so the overlays can be drawn
    /// over it without the list threading a rect out for all 201 rows.
    editing_rect: Option<Rect>,
    request: Option<Request>,
}

/// Draw the brushes in scope.
///
/// `detail` turns on the second line and the per-row management controls, which
/// is the browser's shape; the panel passes `false` and gets the design's
/// compact row.
fn list(
    ui: &mut Ui,
    p: &Palette,
    ed: &Editor,
    state: &State,
    height: f32,
    detail: bool,
    editing: Option<&str>,
) -> ListOut {
    let query = state.query.trim().to_lowercase();
    let index = Arc::clone(&state.index);
    let mut out = ListOut::default();
    let mut any = false;

    for i in visit(&index, &ed.presets, &state.scope, &query) {
        any = true;
        let preset = &ed.presets[i];
        let user = index.is_user(i);
        let response = widgets::brush_row(
            ui,
            p,
            BrushRow {
                name: &preset.name,
                detail: if detail {
                    index.details.get(i).map_or("", String::as_str)
                } else {
                    ""
                },
                brush: &preset.brush,
                // Resolved per row, but only for the handful of presets that
                // name a stamp: `tip` is `None` on every round brush, so this
                // is a null check on two hundred of the two hundred and one
                // rows and one map lookup on the rest. Same two-step as
                // `Editor::apply_preset` — the user's library, then Umber's.
                tip: preset
                    .tip
                    .as_deref()
                    .and_then(|name| ed.tips.get(name).or_else(|| umber_core::tip::builtin(name))),
                selected: ed.active_preset == Some(i),
                user,
                height,
                trailing: if detail { ROW_CONTROLS } else { 0.0 },
            },
        );
        if editing == Some(preset.id.as_str()) {
            out.editing_rect = Some(response.rect);
        }

        // The panel has no room for a credit line, so it hangs the attribution
        // off the row. The closure borrows the preset rather than cloning it —
        // `on_hover_ui` has no `'static` bound — so a hundred and thirty-three
        // rows cost nothing to make hoverable.
        let response = if detail {
            response
        } else {
            response.on_hover_ui(|ui| attribution(ui, preset))
        };
        if response.clicked() {
            out.picked = Some(i);
        }
        // Rows scrolled out of sight are not worth two hit tests each.
        if detail
            && editing != Some(preset.id.as_str())
            && ui.is_rect_visible(response.rect)
            && let Some(request) = row_controls(ui, p, response.rect, &preset.id, user)
        {
            out.request = Some(request);
        }
    }

    if !any {
        ui.add_space(6.0);
        controls::note(ui, p, "No brush matches that.");
        ui.add_space(6.0);
    }
    out
}

/// Rename and delete, drawn over the right edge of a browser row.
///
/// Registered *after* the row so they win the hit test against it — egui breaks
/// ties in favour of the last widget added — which is what stops a click on
/// Delete also selecting the brush.
///
/// Both are drawn for every row and disabled on the shipped ones. Hiding them
/// there would leave "why can I not delete this?" for the reader to work out; a
/// dead control that says why is the house style.
fn row_controls(ui: &mut Ui, p: &Palette, rect: Rect, id: &str, user: bool) -> Option<Request> {
    const READ_ONLY: &str = "Brushes that ship with Umber cannot be renamed or deleted. \
                             Save a copy out of the brush editor and change that instead.";
    let marks = [
        (Icon::Pencil, "Rename this brush"),
        (Icon::Trash, "Delete this brush"),
    ];
    let mut request = None;
    for (n, (icon, tip)) in marks.into_iter().enumerate() {
        let hit = Rect::from_center_size(
            pos2(
                rect.right() - ROW_CONTROLS + 10.0 + n as f32 * 22.0,
                rect.center().y,
            ),
            egui::Vec2::splat(18.0),
        );
        let response = ui.interact(
            hit,
            ui.id().with(("brush-row-control", id, n)),
            if user { Sense::click() } else { Sense::hover() },
        );
        let colour = match (user, response.hovered()) {
            (false, _) => p.text_dim.gamma_multiply(0.35),
            (true, true) => p.text_strong,
            (true, false) => p.text_dim,
        };
        icons::draw(ui.painter(), hit.shrink(2.0), icon, colour);
        if response
            .on_hover_text(if user { tip } else { READ_ONLY })
            .clicked()
        {
            request = Some(if n == 0 {
                Request::Rename(id.to_owned())
            } else {
                Request::Confirm(id.to_owned())
            });
        }
    }
    request
}

/// The tooltip the panel's rows carry: what this brush is and who made it.
fn attribution(ui: &mut Ui, preset: &BrushPreset) {
    ui.label(RichText::new(&preset.name).strong());
    ui.label(RichText::new(collection_of(preset)).small().weak());
    let Some(credit) = &preset.credit else {
        return;
    };
    ui.label(RichText::new(format!("{} — {}", credit.author, credit.licence)).small());
    if !credit.source.is_empty() {
        ui.label(RichText::new(&credit.source).small().weak());
    }
    if requires_attribution(&credit.licence) {
        ui.label(RichText::new("Credit the author in work you publish with it.").small());
    }
}

// ---------------------------------------------------------------------------
// Saving out of the editor
// ---------------------------------------------------------------------------

/// The design's "Save brush" footer, for the brush editor dialog.
///
/// The design draws Cancel and Save at the foot of the editor. Umber's editor
/// applies every change live — deliberately: a paint app should show a change
/// as you make it — so there is nothing for Cancel to undo and it is not drawn.
/// What is left is the half that does something: name what you have made, or
/// write it back over the brush you started from.
pub fn save_row(ui: &mut Ui, p: &Palette, ed: &mut Editor) {
    let mut state = load(ui.ctx(), ed);

    ui.add_space(14.0);
    let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(line, 0.0, p.border);
    ui.add_space(10.0);

    if state.saving.is_none()
        && let Some(notice) = state.notice.clone()
        && notice_bar(ui, p, &notice, true)
    {
        state.notice = None;
    }

    if save_field(ui, p, ed, &mut state, SaveSite::Editor) {
        store(ui.ctx(), state);
        return;
    }

    // Updating in place is offered only for a brush that is actually yours: the
    // shipped library is read-only, and a button that says otherwise is a lie
    // the user finds out about after pressing it.
    let existing = match ed.active_preset {
        Some(i) if state.index.is_user(i) => ed
            .presets
            .get(i)
            .map(|preset| (preset.id.clone(), preset.name.clone())),
        _ => None,
    };
    let writable = state.writable();
    let why_not = state.why_not().to_owned();

    ui.horizontal(|ui| {
        controls::note(
            ui,
            p,
            "Changes here reach the brush in your hand straight away. \
             Saving is what keeps them.",
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| match &existing {
            Some((id, name)) => {
                if controls::text_button(ui, p, &format!("Update \"{name}\""), true, writable)
                    .on_hover_text(if writable {
                        "Write these settings back over the saved brush"
                    } else {
                        why_not.as_str()
                    })
                    .clicked()
                {
                    update(&mut state, ed, id.clone());
                }
                if controls::text_button(ui, p, "Save as new…", false, writable)
                    .on_hover_text(if writable {
                        "Keep the saved brush as it is and add another"
                    } else {
                        why_not.as_str()
                    })
                    .clicked()
                {
                    state.saving = Some((SaveSite::Editor, Field::new(suggested_name(ed))));
                }
            }
            None => {
                if controls::text_button(ui, p, "Save brush…", true, writable)
                    .on_hover_text(if writable {
                        "Add this brush to your library"
                    } else {
                        why_not.as_str()
                    })
                    .clicked()
                {
                    state.saving = Some((SaveSite::Editor, Field::new(suggested_name(ed))));
                }
            }
        });
    });

    store(ui.ctx(), state);
}

/// The name field, when a save has been armed from `site`. Returns whether it
/// was drawn, so the caller can leave out the buttons that armed it.
fn save_field(
    ui: &mut Ui,
    p: &Palette,
    ed: &mut Editor,
    state: &mut State,
    site: SaveSite,
) -> bool {
    match &state.saving {
        Some((armed, _)) if *armed == site => {}
        _ => return false,
    }

    let Some((_, field)) = &mut state.saving else {
        return false;
    };
    let mut name = std::mem::take(&mut field.text);
    let focus = std::mem::take(&mut field.focus);
    let mut commit = false;
    let mut cancel = false;

    Frame::NONE
        .fill(p.window)
        .stroke(Stroke::new(1.0, p.accent_dim))
        .corner_radius(metrics::RADIUS)
        .inner_margin(Margin::symmetric(8, 7))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.label(
                RichText::new("Save this brush as")
                    .size(text::TINY)
                    .color(p.text_dim),
            );
            let edit = ui.add(
                TextEdit::singleline(&mut name)
                    .id(Id::new("brushlib-save-name"))
                    .desired_width(ui.available_width())
                    .font(FontId::proportional(text::CONTROL))
                    .text_color(p.text_strong),
            );
            if focus {
                edit.request_focus();
            }
            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                commit = true;
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                cancel = true;
            }
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let named = !name.trim().is_empty();
                    if controls::text_button(ui, p, "Save", true, named)
                        .on_hover_text(if named {
                            "Add it to your library"
                        } else {
                            "Give the brush a name first."
                        })
                        .clicked()
                    {
                        commit = true;
                    }
                    if controls::text_button(ui, p, "Cancel", false, true).clicked() {
                        cancel = true;
                    }
                });
            });
        });
    ui.add_space(8.0);

    if cancel {
        state.saving = None;
    } else if commit && !name.trim().is_empty() {
        save_new(state, ed, name.trim().to_owned());
    } else if let Some((_, field)) = &mut state.saving {
        field.text = name;
    }
    true
}

/// What a new brush should be called before the user overtypes it.
///
/// A brush derived from a shipped preset is suggested as a copy of it, which is
/// what it is — two rows both called "Soft round", one of them yours, is a list
/// you cannot read.
fn suggested_name(ed: &Editor) -> String {
    match ed.active_preset.and_then(|i| ed.presets.get(i)) {
        Some(preset) => format!("{} copy", preset.name),
        None => "My brush".to_owned(),
    }
}

fn active_name(ed: &Editor) -> String {
    ed.active_preset
        .and_then(|i| ed.presets.get(i))
        .map_or_else(|| "Brush".to_owned(), |preset| preset.name.clone())
}

fn save_new(state: &mut State, ed: &mut Editor, name: String) {
    let brush = ed.brush;
    // A brand-new preset names no tip, so the mask has to travel with it or a
    // stamp brush would be saved as a round one. Cloned out of the `Arc`: the
    // library stores a copy of its own and hands out a fresh handle.
    let tip = ed.tip.as_deref().cloned();
    let label = name.clone();
    let saved = write(state, ed, move |library| {
        library.save(BrushPreset::unsaved(name, brush), tip)
    });
    if let Some(id) = saved {
        // Select what was just saved. The user has named this brush; leaving
        // the selection on the preset it came from would send the very next
        // edit — and any Update — to the wrong place.
        //
        // Applied rather than just pointed at, so `Editor::tip` becomes the
        // library's copy of the mask. Both are the same picture; using one
        // handle means the renderer sees no change at the next stroke.
        if let Some(index) = ed.presets.iter().position(|preset| preset.id == id) {
            ed.apply_preset(index);
        }
        state.saving = None;
        state.notice = Some(Notice::good(format!("Saved \"{label}\" to your library.")));
    }
}

fn update(state: &mut State, ed: &mut Editor, id: String) {
    let Some(mut preset) = ed.presets.iter().find(|p| p.id == id).cloned() else {
        return;
    };
    preset.brush = ed.brush;
    // The tip is part of what "these settings" means. Taking one off is
    // clearing the reference; putting a different one on is an import, which
    // arrives as its own brush. Neither goes through here, so the mask itself
    // never has to be rewritten.
    let tip_removed = ed.tip.is_none() && preset.tip.is_some();
    if tip_removed {
        preset.tip = None;
    }
    let label = preset.name.clone();
    if write(state, ed, move |library| library.save(preset, None)).is_some() {
        state.notice = Some(Notice::good(if tip_removed {
            format!("Updated \"{label}\", and took its bitmap tip off.")
        } else {
            format!("Updated \"{label}\".")
        }));
    }
}

// ---------------------------------------------------------------------------
// Importing
// ---------------------------------------------------------------------------

/// Pick brush files and read them into the library.
///
/// Blocking, like the PNG export dialog it copies: a native file picker on the
/// UI thread is what every desktop app does, and the alternative is a channel
/// and a state machine for something the user is standing over anyway.
fn import(state: &mut State, ed: &mut Editor) {
    if !state.writable() {
        return;
    }
    // Every extension `umber_core::brushimport::read_file` claims, in one
    // filter first so that a folder of mixed formats — which is what a brush
    // pack actually is — can be selected in one go.
    let Some(paths) = rfd::FileDialog::new()
        .set_title("Import brushes")
        .add_filter(
            "Brush files",
            &[
                "myb", "gbr", "gpb", "gih", "vbr", "kpp", "bundle", "abr", "sut", "sutg", "ron",
            ],
        )
        .add_filter("MyPaint brush", &["myb"])
        .add_filter("GIMP brush", &["gbr", "gpb", "gih", "vbr"])
        .add_filter("Krita brush", &["kpp", "bundle"])
        .add_filter("Photoshop brush", &["abr"])
        .add_filter("Clip Studio sub-tool", &["sut", "sutg"])
        .add_filter("Umber brush library", &["ron"])
        // Deliberately present, and deliberately last: picking the wrong kind
        // of file gives a sentence naming it and the reason, which is a better
        // answer than a picker that refuses to show the file at all.
        .add_filter("All files", &["*"])
        .pick_files()
    else {
        return;
    };

    let mut added: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    let mut dropped: Vec<&'static str> = Vec::new();
    let mut first: Option<String> = None;
    for path in &paths {
        // Asked before the read, so a file that fails is reported once, as a
        // failure, rather than also being complained about for what it dropped.
        let losses = umber_core::brushimport::dropped_features(path);
        match write(state, ed, |library| library.import_file(path)) {
            Some(presets) => {
                first = first.or_else(|| presets.first().map(|preset| preset.id.clone()));
                added.extend(presets.into_iter().map(|preset| preset.name));
                for loss in losses {
                    if !dropped.contains(&loss) {
                        dropped.push(loss);
                    }
                }
            }
            // `write` has already put the reason in the notice; it is collected
            // here so that twenty dropped files report as twenty results rather
            // than as whichever one happened to be last.
            None => failures.push(state.notice.as_ref().map_or_else(
                || format!("{} could not be read.", file_label(path)),
                |notice| notice.text.clone(),
            )),
        }
    }

    state.notice = Some(import_notice(&paths, &added, &failures, &dropped));
    // Anything that arrived is worth looking at, and it will not be in whatever
    // collection happened to be showing.
    if !added.is_empty() {
        state.scope = Scope::All;
        state.query.clear();
    }
    // And put the first of them in the user's hand. Importing a brush is asking
    // to paint with it; a stamp brush in particular is unrecognisable in a list
    // and obvious the moment it makes a mark. Selecting it is also what binds
    // its tip, since `apply_preset` is where a tip reference is resolved.
    if let Some(index) = first.and_then(|id| ed.presets.iter().position(|p| p.id == id)) {
        ed.apply_preset(index);
    }
}

/// Say what actually arrived — and what did not survive the trip.
///
/// One file can hold a whole library and several files can each fail
/// differently, so both the count and the reasons matter. `dropped` names the
/// features Umber cannot render: those brushes *are* imported, because an
/// approximation of your own brush beats a refusal, but they will not paint
/// quite like the originals and saying so is the difference between an
/// approximation and a bug report.
fn import_notice(
    paths: &[PathBuf],
    added: &[String],
    failures: &[String],
    dropped: &[&str],
) -> Notice {
    let summary = match (added.len(), paths.len()) {
        (0, _) => String::new(),
        (1, 1) => format!("Imported \"{}\" from {}.", added[0], file_label(&paths[0])),
        (n, 1) => format!("Imported {n} brushes from {}.", file_label(&paths[0])),
        (n, f) => format!("Imported {n} brushes from {f} files."),
    };
    // "Umber has no …" was the wording until a container could report six
    // losses at once, several of which are things Umber *does* have and simply
    // could not find — a tip stored outside the file, say. This frame is true
    // of every entry any reader produces.
    let losses = match dropped.len() {
        0 => String::new(),
        _ => format!(
            " Umber could not bring across {}, so {} will paint differently.",
            join_words(dropped),
            if added.len() == 1 {
                "it"
            } else {
                "some of them"
            },
        ),
    };
    let trailer = match failures.len() {
        0 => {
            return {
                if summary.is_empty() {
                    Notice::bad("Those files held no brushes.")
                } else if losses.is_empty() {
                    Notice::good(summary)
                } else {
                    // Not an error — the brushes are there — but not the plain
                    // success a green line would claim either.
                    Notice::bad(format!("{summary}{losses}"))
                }
            };
        }
        1 => failures[0].clone(),
        n => format!("{} — and {} more failed.", failures[0], n - 1),
    };
    Notice::bad(if summary.is_empty() {
        trailer
    } else {
        format!("{summary}{losses} {trailer}")
    })
}

/// `smudge`, `smudge and tilt`, `smudge, tilt and direction`.
fn join_words(words: &[&str]) -> String {
    match words {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

// ---------------------------------------------------------------------------
// The browser
// ---------------------------------------------------------------------------

/// Draw the library's own windows.
///
/// Called from [`crate::panels::sidebars`], which is the one entry point
/// `ui.rs` runs every frame whatever the layout is doing. A modal drawn from
/// inside the Brushes panel body would vanish the moment that panel was
/// hidden — with no way to shut it and no way to get it back.
pub fn dialogs(root: &mut Ui, p: &Palette, ed: &mut Editor) {
    let mut state = load(root.ctx(), ed);
    browser(root, p, ed, &mut state);
    store(root.ctx(), state);
}

fn browser(root: &mut Ui, p: &Palette, ed: &mut Editor, state: &mut State) {
    if !state.browser_open {
        return;
    }
    // Clamped to the window, because a modal wider than the screen has no way
    // back out of its own corners.
    let available = root.ctx().content_rect().size();
    let [full_width, full_height] = metrics::BRUSH_LIBRARY;
    let width = full_width.min(available.x - 48.0).max(460.0);
    let height = full_height.min(available.y - 48.0).max(300.0);

    let response = egui::Modal::new(Id::new("brush-library-browser"))
        .frame(
            Frame::NONE
                .fill(p.window)
                .stroke(Stroke::new(1.0, p.popover_border))
                .corner_radius(10)
                .inner_margin(Margin::ZERO),
        )
        .show(root.ctx(), |ui| {
            ui.set_width(width);
            ui.set_height(height);
            ui.horizontal_top(|ui| {
                // No gutter: the rail's own fill provides the hairline between
                // it and the pane, as in the settings dialog.
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.allocate_ui_with_layout(
                    vec2(metrics::BRUSH_LIBRARY_RAIL, height),
                    Layout::top_down(Align::Min),
                    |ui| browser_rail(ui, p, state),
                );
                ui.allocate_ui_with_layout(
                    vec2(width - metrics::BRUSH_LIBRARY_RAIL, height),
                    Layout::top_down(Align::Min),
                    |ui| browser_pane(ui, p, ed, state),
                );
            });
        });

    if response.should_close() {
        close_browser(state);
    }
}

fn close_browser(state: &mut State) {
    state.browser_open = false;
    state.renaming = None;
    state.confirming = None;
}

fn browser_rail(ui: &mut Ui, p: &Palette, state: &mut State) {
    Frame::NONE
        .fill(p.chrome)
        .inner_margin(Margin::symmetric(8, 16))
        .show(ui, |ui| {
            ui.set_min_height(ui.available_height());
            ui.spacing_mut().item_spacing.y = 2.0;

            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(
                    RichText::new("Collections")
                        .size(text::HEADING)
                        .color(p.text_strong)
                        .strong(),
                );
            });
            ui.add_space(12.0);

            let mut chosen = None;
            let all = format!("All brushes ({})", state.index.total);
            if controls::sidebar_tab(ui, p, &all, state.scope == Scope::All, true, "").clicked() {
                chosen = Some(Scope::All);
            }
            ui.add_space(8.0);

            egui::ScrollArea::vertical()
                .id_salt("brush-library-collections")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for group in &state.index.groups {
                        let scope = Scope::Category(group.name.clone());
                        let label = format!("{} ({})", group.name, group.members.len());
                        if controls::sidebar_tab(ui, p, &label, state.scope == scope, true, "")
                            .clicked()
                        {
                            chosen = Some(scope);
                        }
                    }
                });

            if let Some(scope) = chosen {
                state.scope = scope;
            }
        });
}

fn browser_pane(ui: &mut Ui, p: &Palette, ed: &mut Editor, state: &mut State) {
    Frame::NONE
        .inner_margin(Margin::symmetric(22, 18))
        .show(ui, |ui| {
            ui.set_min_height(ui.available_height());
            ui.spacing_mut().item_spacing.y = 8.0;

            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new("Brush library")
                            .size(15.0)
                            .color(p.text_strong)
                            .strong(),
                    );
                    controls::note(
                        ui,
                        p,
                        "Everything Umber ships with, and everything you have saved. \
                         Click one to paint with it.",
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                    if icon_button(ui, p, Icon::Close, true, "Close") {
                        close_browser(state);
                    }
                });
            });
            ui.add_space(8.0);

            let writable = state.writable();
            let why_not = state.why_not().to_owned();
            ui.horizontal(|ui| {
                ui.scope(|ui| {
                    ui.set_width(300.0);
                    controls::search_field(ui, p, &mut state.query, "Search brushes");
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if controls::text_button(ui, p, "Import…", true, writable)
                        .on_hover_text(if writable {
                            "Read MyPaint, GIMP, Krita or Photoshop brushes, or an Umber .ron library"
                        } else {
                            why_not.as_str()
                        })
                        .clicked()
                    {
                        import(state, ed);
                    }
                });
            });

            if let Store::Broken(why) = &state.store {
                let why = why.clone();
                notice_bar(ui, p, &Notice::bad(why), false);
            }
            if let Some(notice) = state.notice.clone()
                && notice_bar(ui, p, &notice, true)
            {
                state.notice = None;
            }

            // Room for the footer, taken before the list, so the list gets
            // whatever is left rather than pushing the footer off the bottom.
            let list_height = (ui.available_height() - 34.0).max(80.0);
            browser_list(ui, p, ed, state, list_height);

            ui.add_space(4.0);
            browser_footer(ui, p, ed, state);
        });
}

fn browser_list(ui: &mut Ui, p: &Palette, ed: &mut Editor, state: &mut State, height: f32) {
    // Whichever row is being edited, so the list can report where it landed.
    let editing = state
        .renaming
        .as_ref()
        .map(|(id, _)| id.clone())
        .or_else(|| state.confirming.clone());
    let mut renaming = state.renaming.clone();
    let mut out = ListOut::default();

    Frame::NONE
        .fill(p.window)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(metrics::RADIUS_LARGE)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("brush-library-list")
                .max_height(height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    out = list(
                        ui,
                        p,
                        ed,
                        state,
                        metrics::BRUSH_ROW_DETAIL,
                        true,
                        editing.as_deref(),
                    );
                    // The two rows that are not brush rows: the one being
                    // renamed and the one being deleted. Drawn after the list
                    // so they land over the row they belong to.
                    let Some(rect) = out.editing_rect else { return };
                    if let Some((id, field)) = &mut renaming {
                        if let Some(request) = rename_overlay(ui, p, rect, id, field) {
                            out.request = Some(request);
                        }
                    } else if let Some(id) = &state.confirming
                        && let Some(request) = confirm_overlay(ui, p, rect, id)
                    {
                        out.request = Some(request);
                    }
                });
        });

    state.renaming = renaming;
    match out.request {
        Some(Request::Rename(id)) => {
            let name = name_of(ed, &id);
            state.renaming = Some((id, Field::new(name)));
            state.confirming = None;
        }
        Some(Request::Confirm(id)) => {
            state.confirming = Some(id);
            state.renaming = None;
        }
        Some(Request::Cancel) => {
            state.renaming = None;
            state.confirming = None;
        }
        Some(Request::Commit(id, name)) => {
            state.renaming = None;
            if write(state, ed, |library| library.rename(&id, &name)).is_some() {
                state.notice = Some(Notice::good(format!("Renamed to \"{name}\".")));
            }
        }
        Some(Request::Delete(id)) => {
            let name = name_of(ed, &id);
            state.confirming = None;
            if write(state, ed, |library| library.delete(&id)).is_some() {
                state.notice = Some(Notice::good(format!("Deleted \"{name}\".")));
            }
        }
        None => {
            if let Some(index) = out.picked {
                ed.apply_preset(index);
            }
        }
    }
}

fn name_of(ed: &Editor, id: &str) -> String {
    ed.presets
        .iter()
        .find(|preset| preset.id == id)
        .map_or_else(String::new, |preset| preset.name.clone())
}

/// The row being renamed, as a field over the top of it.
///
/// The row underneath is still drawn — sample, credit and all — so the rename
/// reads as editing that brush rather than as a dialog that appeared from
/// nowhere.
fn rename_overlay(
    ui: &mut Ui,
    p: &Palette,
    rect: Rect,
    id: &str,
    field: &mut Field,
) -> Option<Request> {
    ui.painter()
        .rect_filled(rect, metrics::RADIUS, p.control_active);
    let inner = Rect::from_min_max(
        pos2(rect.left() + 82.0, rect.center().y - 12.0),
        pos2(rect.right() - 8.0, rect.center().y + 12.0),
    );
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::left_to_right(Align::Center)),
    );

    let focus = std::mem::take(&mut field.focus);
    let edit = child.add(
        TextEdit::singleline(&mut field.text)
            .id(Id::new("brushlib-rename"))
            .desired_width((inner.width() - 52.0).max(60.0))
            .font(FontId::proportional(text::CONTROL))
            .text_color(p.text_strong),
    );
    if focus {
        edit.request_focus();
    }

    let named = !field.text.trim().is_empty();
    let entered = edit.lost_focus() && child.input(|i| i.key_pressed(egui::Key::Enter));
    let confirmed = icon_button(
        &mut child,
        p,
        Icon::Check,
        named,
        if named {
            "Rename it"
        } else {
            "Give the brush a name first."
        },
    );
    let abandoned = icon_button(&mut child, p, Icon::Close, true, "Keep the old name")
        || child.input(|i| i.key_pressed(egui::Key::Escape));

    if abandoned {
        return Some(Request::Cancel);
    }
    if (confirmed || entered) && named {
        return Some(Request::Commit(id.to_owned(), field.text.trim().to_owned()));
    }
    None
}

/// The row whose Delete has been pressed once.
///
/// Deleting a brush is not undoable — Umber's history covers painting only — so
/// the second press is asked for rather than assumed.
fn confirm_overlay(ui: &mut Ui, p: &Palette, rect: Rect, id: &str) -> Option<Request> {
    let painter = ui.painter();
    painter.rect_filled(rect, metrics::RADIUS, p.warning_bg);
    painter.rect_stroke(
        rect,
        metrics::RADIUS,
        Stroke::new(1.0, p.warning_border),
        egui::StrokeKind::Inside,
    );
    painter.text(
        pos2(rect.left() + 12.0, rect.center().y),
        Align2::LEFT_CENTER,
        "Delete this brush? It cannot be brought back.",
        FontId::proportional(text::CONTROL),
        p.warning,
    );

    let inner = Rect::from_min_max(
        pos2(rect.right() - 170.0, rect.center().y - 12.0),
        pos2(rect.right() - 10.0, rect.center().y + 12.0),
    );
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::right_to_left(Align::Center)),
    );
    if controls::text_button(&mut child, p, "Delete", true, true).clicked() {
        return Some(Request::Delete(id.to_owned()));
    }
    if controls::text_button(&mut child, p, "Keep", false, true).clicked()
        || child.input(|i| i.key_pressed(egui::Key::Escape))
    {
        return Some(Request::Cancel);
    }
    None
}

fn browser_footer(ui: &mut Ui, p: &Palette, ed: &Editor, state: &State) {
    let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(line, 0.0, p.border);
    ui.add_space(6.0);

    let saved = ed.presets.len().saturating_sub(state.index.shipped);
    let path = state
        .dir()
        .map_or_else(|| "nowhere".to_owned(), |path| path.display().to_string());
    ui.horizontal(|ui| {
        ui.add(
            egui::Label::new(
                RichText::new(format!("Your brushes are saved in {path}"))
                    .size(10.0)
                    .color(p.text_dim),
            )
            .truncate(),
        )
        .on_hover_text(path.as_str());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(format!("{} shipped · {saved} yours", state.index.shipped))
                    .size(10.0)
                    .color(p.text_dim),
            );
        });
    });
}

// ---------------------------------------------------------------------------
// Small painted pieces
// ---------------------------------------------------------------------------

/// An inset strip carrying something that happened, and the way to dismiss it.
///
/// `controls::banner` lays its message out on one line, which is right in a
/// 1000 px dialog and wrong in a 264 px panel; this wraps. Returns whether the
/// dismiss mark was clicked.
fn notice_bar(ui: &mut Ui, p: &Palette, notice: &Notice, dismissable: bool) -> bool {
    let mut dismissed = false;
    Frame::NONE
        .fill(if notice.bad { p.warning_bg } else { p.window })
        .stroke(Stroke::new(
            1.0,
            if notice.bad {
                p.warning_border
            } else {
                p.accent_dim
            },
        ))
        .corner_radius(metrics::RADIUS)
        .inner_margin(Margin::symmetric(8, 6))
        .show(ui, |ui| {
            let full = ui.available_width();
            ui.horizontal_top(|ui| {
                ui.set_min_width(full);
                ui.scope(|ui| {
                    // The width the text has to live inside, stated rather than
                    // discovered. A label in a horizontal layout defaults to
                    // `TextWrapMode::Extend`, so this used to size the strip
                    // instead of being sized by it — and an import that lost
                    // eight features put the browser wider than the screen,
                    // with its corners out of reach. `set_max_width` alone is
                    // not enough: it bounds the ui, and an extending label
                    // simply overruns it.
                    ui.set_max_width((full - 26.0).max(40.0));
                    ui.add(
                        egui::Label::new(
                            RichText::new(&notice.text)
                                .size(text::TINY)
                                .color(if notice.bad { p.warning } else { p.text })
                                .line_height(Some(14.0)),
                        )
                        .wrap(),
                    );
                });
                if dismissable {
                    ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                        if icon_button(ui, p, Icon::Close, true, "Dismiss") {
                            dismissed = true;
                        }
                    });
                }
            });
        });
    dismissed
}

/// An icon followed by a label, behaving as one clickable unit.
///
/// `ui.rs` has one of these but keeps it private, and that file belongs to the
/// workspace rather than to this feature. This one takes the whole row width,
/// elides a label that does not fit, and draws a disabled state — which the
/// library needs, because a broken library still has to explain itself.
fn link(ui: &mut Ui, p: &Palette, icon: Icon, label: &str, enabled: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), 18.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let colour = match (enabled, response.hovered()) {
        (false, _) => p.text_dim.gamma_multiply(0.45),
        (true, true) => p.text_strong,
        (true, false) => p.text_dim,
    };
    let painter = ui.painter();
    icons::draw(
        painter,
        Rect::from_min_size(rect.left_top(), vec2(14.0, 18.0)),
        icon,
        colour,
    );
    painter.text(
        pos2(rect.left() + 18.0, rect.center().y),
        Align2::LEFT_CENTER,
        widgets::elide(painter, label, text::TINY, rect.width() - 20.0),
        FontId::proportional(text::TINY),
        colour,
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(id: &str, name: &str, category: &str) -> BrushPreset {
        BrushPreset {
            id: id.to_owned(),
            name: name.to_owned(),
            category: category.to_owned(),
            credit: None,
            brush: umber_core::Brush::default(),
            tip: None,
        }
    }

    #[test]
    fn the_search_folds_case_without_allocating_a_lowered_copy() {
        assert!(contains_ignore_case("Soft Round", "round"));
        assert!(contains_ignore_case("Soft Round", "SOFT"));
        assert!(!contains_ignore_case("Soft", "softer"));
        // A query longer than the name must not index past the end.
        assert!(!contains_ignore_case("", "x"));
        assert!(contains_ignore_case("anything", ""));
    }

    #[test]
    fn a_search_matches_the_collection_as_well_as_the_name() {
        let p = preset("mypaint/x", "Bulk 1", style::Style::TEXTURE);
        assert!(matches(&p, "texture"));
        assert!(matches(&p, "bulk"));
        assert!(!matches(&p, "charcoal"));
    }

    /// The whole point of the grouping: 239 presets under one heading is the
    /// flat list this replaces.
    #[test]
    fn collections_run_yours_first_then_styles_in_their_declared_order() {
        let shipped = preset::builtin().len();
        let mut presets = preset::builtin().to_vec();
        presets.push(preset("user/mine", "Mine", "My brushes"));
        let index = Index::build(&presets);

        assert_eq!(index.groups[0].name, "My brushes");
        // Then `Style::ALL` order, which is roughly the order a painter works
        // in — not alphabetical, which would open the library on "Airbrush".
        let styles: Vec<&str> = index.groups[1..].iter().map(|g| g.name.as_str()).collect();
        let expected: Vec<&str> = style::Style::ALL
            .iter()
            .copied()
            .filter(|s| styles.contains(s))
            .collect();
        assert_eq!(styles, expected);

        assert!(index.is_user(shipped));
        assert!(!index.is_user(shipped - 1));
        assert_eq!(index.total, presets.len());
        // Every preset lands in exactly one collection, or the picker would
        // quietly hide brushes.
        let members: usize = index.groups.iter().map(|g| g.members.len()).sum();
        assert_eq!(members, presets.len());
    }

    /// A library grouped by author put the pencils in six places. This is the
    /// guard against sliding back into that: no collection may be a person.
    #[test]
    fn no_shipped_collection_is_named_after_whoever_drew_it() {
        for preset in preset::builtin() {
            assert!(
                style::Style::ALL.contains(&preset.category.as_str()),
                "{:?} is in {:?}, which is not a style",
                preset.name,
                preset.category
            );
        }
    }

    #[test]
    fn every_shipped_preset_has_a_credit_line_to_show() {
        let index = Index::build(preset::builtin());
        for (i, preset) in preset::builtin().iter().enumerate() {
            assert!(
                !index.details[i].is_empty(),
                "{} has nothing to put on its credit row",
                preset.name
            );
        }
    }

    /// The cautious direction is the safe one: a licence this does not
    /// recognise is treated as needing credit.
    #[test]
    fn attribution_is_assumed_unless_the_licence_waives_it() {
        assert!(!requires_attribution("CC0-1.0"));
        assert!(!requires_attribution("Public Domain"));
        assert!(!requires_attribution(""));
        assert!(requires_attribution("CC-BY-4.0"));
        assert!(requires_attribution("GPL-3.0-or-later"));
    }

    #[test]
    fn an_import_says_how_many_brushes_arrived_and_from_where() {
        let one = PathBuf::from("packs/charcoal.myb");
        let notice = import_notice(
            std::slice::from_ref(&one),
            &["Charcoal".to_owned()],
            &[],
            &[],
        );
        assert!(!notice.bad);
        assert!(notice.text.contains("Charcoal"), "{}", notice.text);
        assert!(notice.text.contains("charcoal.myb"), "{}", notice.text);

        let many = import_notice(
            std::slice::from_ref(&one),
            &["A".to_owned(), "B".to_owned()],
            &[],
            &[],
        );
        assert!(many.text.starts_with("Imported 2 brushes"), "{}", many.text);

        // A failure is never only logged, and never swallowed by a success.
        let mixed = import_notice(
            &[one.clone(), PathBuf::from("nope.kpp")],
            &["A".to_owned()],
            &["nope.kpp is not a brush file Umber can read".to_owned()],
            &[],
        );
        assert!(mixed.bad);
        assert!(mixed.text.contains("nope.kpp"), "{}", mixed.text);
        assert!(mixed.text.contains("Imported 1"), "{}", mixed.text);
    }

    #[test]
    fn several_failures_are_counted_rather_than_lost() {
        let notice = import_notice(
            &[PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c")],
            &[],
            &["a failed".to_owned(), "b failed".to_owned(), "c".to_owned()],
            &[],
        );
        assert!(notice.bad);
        assert!(notice.text.contains("2 more failed"), "{}", notice.text);
    }

    /// The brush arrives, and is *said* to be an approximation. Shipping it
    /// silently is the failure this guards: the user would find out by painting
    /// with it and concluding the importer is broken.
    #[test]
    fn an_import_that_lost_something_says_so_without_calling_it_a_failure() {
        let path = PathBuf::from("packs/smudger.myb");
        let notice = import_notice(
            std::slice::from_ref(&path),
            &["Smudger".to_owned()],
            &[],
            &["smudge"],
        );
        assert!(notice.text.contains("Imported"), "{}", notice.text);
        assert!(notice.text.contains("smudge"), "{}", notice.text);
        assert!(notice.text.contains("paint differently"), "{}", notice.text);

        // Losses survive alongside a real failure rather than one hiding the
        // other — they are answers to different questions.
        let both = import_notice(
            &[path, PathBuf::from("nope.kpp")],
            &["Smudger".to_owned()],
            &["nope.kpp could not be read".to_owned()],
            &["smudge"],
        );
        assert!(both.text.contains("smudge"), "{}", both.text);
        assert!(both.text.contains("nope.kpp"), "{}", both.text);
    }

    #[test]
    fn several_lost_features_read_as_a_sentence() {
        assert_eq!(join_words(&["smudge"]), "smudge");
        assert_eq!(join_words(&["smudge", "tilt"]), "smudge and tilt");
        assert_eq!(
            join_words(&["smudge", "tilt", "direction"]),
            "smudge, tilt and direction"
        );
    }
}
