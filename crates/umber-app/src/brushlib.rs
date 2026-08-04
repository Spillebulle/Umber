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
//! - **The drag that moves a brush between collections is a model, in
//!   [`crate::brushdrag`].** What a release would do is decided there and
//!   nowhere else, so this file only supplies the pointer and the rectangles
//!   the rail's rows landed in — the same division `dock.rs` keeps against
//!   `panels.rs`. Where the resulting choice is *stored* is
//!   `preset::Library::collections`'s to explain, and it is not obvious: a
//!   shipped brush has no preset that survives an update to write it on.
//! - **Every collection but one is derived from a brush.** [`Index::build`]
//!   reads them off the presets, so a collection exists exactly while something
//!   is filed under it — which leaves no way to make the empty one a brush
//!   would be dragged into first. The rail's `＋` makes one, and it has to be
//!   recorded in `preset::Library::made_collections` rather than derived, or it
//!   would be gone by the next [`resync`].
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

use crate::brushdrag;
use crate::controls;
use crate::editor::{BrushTab, Editor};
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
use umber_core::TipMask;
use umber_core::preset::{self, BrushPreset, NewCollection, PresetError, UserLibrary};
use umber_core::style;

/// Width kept clear at the right of a browser row for its three controls —
/// edit, rename and delete. See [`row_controls`], which spaces them 22 apart
/// from 10 inside this margin.
const ROW_CONTROLS: f32 = 68.0;

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
    /// The "Save this brush as" field, while it is up. One place arms it now —
    /// the brush editor's footer — where there used to be two, so it no longer
    /// has to carry which.
    saving: Option<Field>,
    /// The id of the user brush being renamed, and the name so far.
    renaming: Option<(String, Field)>,
    /// The id of the user brush whose Delete has been pressed once. Deleting a
    /// brush cannot be undone — the history covers painting only — so it asks.
    confirming: Option<String>,
    /// The name of the collection being made, while the rail's field is up.
    creating: Option<Field>,
    /// The brush being carried from one collection to another, if any.
    /// [`crate::brushdrag`] decides what a release would do; this only holds
    /// the answer between frames.
    drag: Option<brushdrag::Drag>,
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

/// The collections the user has made, which have no members to be derived from
/// and so cannot come out of `Editor::presets`. See
/// `preset::Library::made_collections`.
///
/// A free function rather than a method because [`Index::build`] is handed the
/// store's contents while the state around it is being replaced.
fn made_of(store: &Store) -> &[String] {
    match store {
        Store::Ready(library) => library.made_collections(),
        Store::Broken(_) => &[],
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
            let index = Index::build(&ed.presets, made_of(&state.store));
            state.index = Arc::new(index);
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
        index: Arc::new(Index::build(&ed.presets, made_of(&store))),
        store,
        query: String::new(),
        scope: Scope::All,
        browser_open: false,
        saving: None,
        renaming: None,
        confirming: None,
        creating: None,
        drag: None,
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
    /// `made` is the collections the user created, which have no members yet
    /// and so cannot be derived from `presets` — that is the whole reason
    /// `preset::Library::made_collections` is written down.
    fn build(presets: &[BrushPreset], made: &[String]) -> Self {
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
        // A collection somebody made and has not filled yet, as a group with
        // nothing in it. Added only where no preset has already produced one of
        // the same name: a brush dragged into a made collection derives it too,
        // and two rows of one name would be two places to drop a brush and one
        // place to look for it. `same_collection` rather than `==` for the same
        // reason `create_collection` refuses on it — the comparison that
        // decides a clash and the one that merges the row have to agree.
        for name in made {
            if !groups
                .iter()
                .any(|group| preset::same_collection(&group.name, name))
            {
                groups.push(Group {
                    name: name.clone(),
                    members: Vec::new(),
                });
            }
        }
        // Yours first, then the styles in the order `umber_core::style`
        // declares them — roughly the order a painter works in, drawing media
        // through paint to the things done to paint already down. A library you
        // cannot add to is a reference; the brushes you made are the ones you
        // are reaching for, so they stay at the top.
        groups.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.name.cmp(&b.name)));
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

/// The heading this brush sits under: what the user put it in, or failing that
/// what it arrived with.
fn collection_of(preset: &BrushPreset) -> &str {
    match preset.collection() {
        "" => "Uncategorised",
        name => name,
    }
}

/// Sort key for a collection: yours first, then the styles in their declared
/// order.
///
/// "Yours" is decided by the *name*, not by who owns the brushes in it. A
/// collection the classifier could never have produced — "My brushes",
/// "Imported", a name somebody typed, a name an imported library brought — is
/// one somebody chose, and those are the ones being reached for.
///
/// Reading it off the members instead ("every brush in here is the user's")
/// looks equivalent and is not: dragging one shipped brush into "My brushes"
/// would make the group no longer all yours, and send the collection you use
/// most to the bottom of the rail.
fn rank(group: &Group) -> usize {
    match style::order_of(&group.name) {
        // `order_of` answers with the length for a name it does not know.
        unknown if unknown == style::Style::ALL.len() => 0,
        order => 1 + order,
    }
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
///
/// The collection searched is the one the row is *filed under*, not the style
/// underneath it: a brush the user has moved has to be findable where they put
/// it, and a search that still answered to the old heading would be pointing at
/// a row that is not there.
fn matches(preset: &BrushPreset, query: &str) -> bool {
    query.is_empty()
        || contains_ignore_case(&preset.name, query)
        || contains_ignore_case(collection_of(preset), query)
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
            (true, true) => collection_of(preset).to_owned(),
        },
        // A brush the user made or imported carries no credit, and inventing
        // one would be worse than saying where it sits.
        None => collection_of(preset).to_owned(),
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
    // Rebuilt from the shipped library rather than truncated back to it: a
    // shipped preset in hand carries whichever collection the user last put it
    // in, and taking that choice off again has to take it off the copy here
    // too. Two hundred and thirty-nine clones, when the library changes — which
    // is a save, a delete, an import or a move, never a frame.
    ed.presets.clear();
    ed.presets.extend(preset::builtin().iter().cloned());
    ed.presets.extend(library.presets().iter().cloned());
    // Where the user filed the *shipped* brushes. It cannot be on the presets
    // themselves — see `preset::Library::collections` — so it is stamped on
    // here, by id, every time the merged list is rebuilt.
    library.apply_collections(&mut ed.presets);
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
            // The made collections come off the library rather than out of the
            // merged list: `resync` rebuilds that from the presets, and an
            // empty collection has none.
            state.index = Arc::new(Index::build(&ed.presets, library.made_collections()));
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

/// The three marks in the Brushes panel's header: new, browse, and edit.
///
/// The design puts a `＋` there and nothing else. The other two are this
/// module's: with 239 presets the panel is a shortlist rather than the library,
/// so something has to open the rest of it, and the way into the brush editor
/// used to be a `✎ Edit "<name>"…` link at the foot of the panel body. A link
/// is what the design draws, and it was wrong on two counts — it sat below a
/// scrolling list, so a panel dragged short hid the only way to change a brush,
/// and it spent a whole row of a 264 px panel on one verb.
///
/// **Three and not four**, and Import is the obvious fourth. It does not fit.
/// These are right-aligned into the header at 18 points each with 6 between,
/// and in layout edit mode the remove mark is drawn beside them; at
/// `dock::limits::SIDEBAR_MIN_WIDTH` — the narrowest a column may be dragged —
/// that group already reaches back past the header's midpoint, and a fourth
/// mark reaches the panel's own title. Marks drawn over a heading at a width
/// somebody can actually drag to is the exact bug `layers_panel_edges_preview`
/// was written for.
///
/// So Import is in the browser, behind the mark beside this one. It is a
/// *labelled* button there, which is what the rule it was left as a link for
/// asks — a mark cannot say which six applications' brushes Umber reads, and a
/// tooltip on a mark nobody hovers is not the same as a word. It is also where
/// an import *lands*: `preset::IMPORTED` files the arrivals in one collection,
/// and the browser is the only place that collection can be seen. See [`panel`]
/// and [`browser_pane`].
pub fn header_controls(ui: &mut Ui, p: &Palette, ed: &mut Editor) {
    let mut state = load(ui.ctx(), ed);
    let writable = state.writable();

    // Right-to-left: added first lands furthest right, next to the close mark,
    // which is where the design draws the `＋`.
    //
    // It used to arm a "Save this brush as" field instead — `＋` meaning "save
    // what is in your hand", which is what `Plus` means nowhere else in this
    // interface: the Layers header, the Palette header and the tab strip all
    // read it as "make a new one". It is that now, and saving what is in your
    // hand is the brush editor's footer, which the pencil two marks along
    // opens.
    if icon_button(
        ui,
        p,
        Icon::Plus,
        writable,
        if writable {
            "Make a brush from Umber's defaults and open it in the brush editor"
        } else {
            state.why_not()
        },
    ) {
        new_brush(&mut state, ed);
    }
    if icon_button(ui, p, Icon::Grid, true, "Browse the whole brush library") {
        state.browser_open = true;
    }
    // Left of the library mark, and the same pencil the link it replaces
    // carried. The brush is named in the tooltip because the mark cannot say
    // which one it would open, and that is the whole of what the link's label
    // was doing.
    if icon_button(
        ui,
        p,
        Icon::Pencil,
        true,
        &format!("Edit \"{}\" in the brush editor", active_name(ed)),
    ) {
        ed.ui.brush_editor_open = true;
    }

    store(ui.ctx(), state);
}

/// The Brushes panel body: search, collection and the shortlist.
///
/// **Nothing under the list.** There used to be a "New brush…" and an "Import
/// brushes…" link at the foot, and both were the mistake the design's
/// `✎ Edit "<name>"…` link already was: the body is a scroll area, so a panel
/// dragged short scrolls the foot out of sight and takes the control with it.
/// New brush is the header's `＋` now. Import is in the browser, behind the
/// header's library mark — see [`header_controls`] for why it is not a fourth
/// mark up there, and [`browser_pane`], which draws it beside its own New brush
/// so the pair stays a pair.
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

    controls::search_field(ui, p, &mut state.query, "Search brushes");
    ui.add_space(5.0);
    collection_row(ui, p, ed, &mut state);
    ui.add_space(3.0);

    let out = list(ui, p, ed, &state, metrics::BRUSH_ROW, false, None);
    if let Some(index) = out.picked {
        ed.apply_preset(index);
    }

    store(ui.ctx(), state);
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

    // The one trigger that carries a figure as well as a name. Full width,
    // because it is the only thing on its line and a picker sized to
    // "All collections" would jump about as the collection changed.
    let members = count.to_string();
    let mut chosen = None;
    widgets::dropdown(
        ui,
        p,
        widgets::Dropdown::new(label)
            .trailing(&members)
            .width(widgets::DropdownWidth::Fill),
        |ui| {
            let all = format!("All collections ({})", state.index.total);
            if ui
                .selectable_label(state.scope == Scope::All, all)
                .clicked()
            {
                chosen = Some(Scope::All);
            }
            ui.separator();
            for group in &state.index.groups {
                let scope = Scope::Category(group.name.clone());
                let label = format!("{} ({})", group.name, group.members.len());
                if ui.selectable_label(state.scope == scope, label).clicked() {
                    chosen = Some(scope);
                }
            }
        },
    );
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
    /// Put this brush in the user's hand and open the brush editor on it.
    Edit(String),
    /// Start renaming this brush.
    Rename(String),
    /// Arm the delete on this brush.
    Confirm(String),
    /// Back out of whatever is in progress.
    Cancel,
    /// Rename it to this.
    Commit(String, String),
    Delete(String),
    /// Pick this brush up: its id, its name, and the collection it is in.
    Grab(String, String, String),
    /// Put down whatever is being carried, wherever the rail says it landed.
    Drop,
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
                // Only in the browser, which is the one place the collections
                // are on screen to be dropped on.
                draggable: detail,
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
        // Picking a brush up and putting it down. egui settles click against
        // drag for us — a press that never moves far enough is a click — so a
        // row that is dragged is never also selected.
        //
        // Both ends come from the row that was pressed, including the release,
        // because egui keeps the drag with the widget it began on however far
        // the pointer travels. That is what lets the rail be a drop target
        // without the rail having to sense anything.
        if response.drag_started() {
            out.request = Some(Request::Grab(
                preset.id.clone(),
                preset.name.clone(),
                collection_of(preset).to_owned(),
            ));
        } else if response.drag_stopped() {
            out.request = Some(Request::Drop);
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
        controls::note(ui, p, empty_message(state));
        ui.add_space(6.0);
    }
    out
}

/// What the list says when it has nothing to show.
///
/// A collection somebody has just made is empty *because* it is new, and "No
/// brush matches that" reads as a search that failed — which would be the one
/// place in the interface saying the feature had not worked. The distinction is
/// exact rather than a guess: a derived collection cannot be empty, because it
/// exists only where a brush is filed under it.
fn empty_message(state: &State) -> &'static str {
    match (&state.scope, state.query.trim().is_empty()) {
        (Scope::Category(_), true) => {
            "Nothing here yet. Drag a brush onto this collection to file it here."
        }
        _ => "No brush matches that.",
    }
}

/// Edit, rename and delete, drawn over the right edge of a browser row.
///
/// Registered *after* the row so they win the hit test against it — egui breaks
/// ties in favour of the last widget added — which is what stops a click on
/// Delete also selecting the brush.
///
/// **The pencil is live on a shipped brush and the other two are not**, and the
/// difference is not an inconsistency: it is exactly what the Brushes panel
/// already does. Editing a shipped brush is something Umber has always allowed
/// — the panel header's pencil opens the editor on whatever is in your hand,
/// shipped or not, and the editor's footer offers "Save as new…" — because the
/// edit does not land on the shipped preset at all. It cannot: `preset::builtin`
/// is `include_str!`'d into the binary and replaced wholesale by every update,
/// so a change written there would vanish silently months later. That is the
/// same fact that makes renaming and deleting one impossible, and it is why
/// those two stay dead with a tooltip saying so rather than being hidden.
/// The browser used to grey the pencil out with them, which said the library's
/// two hundred and one brushes were the ones you could not start from.
///
/// Three marks rather than the two this had, because the pencil now means what
/// the pencil in the panel header means. Renaming is a different verb and wears
/// [`Icon::Rename`]; sharing the pencil between them is what made "edit" read
/// as "you may not edit this".
fn row_controls(ui: &mut Ui, p: &Palette, rect: Rect, id: &str, user: bool) -> Option<Request> {
    const READ_ONLY: &str = "Brushes that ship with Umber are part of the application, so there \
                             is nothing here to rename or delete — an update replaces the \
                             shipped library wholesale. Edit this one and save it as your own; \
                             that copy is yours to rename and delete.";
    let marks = [
        (Icon::Pencil, "Edit this brush in the brush editor", true),
        (Icon::Rename, "Rename this brush", false),
        (Icon::Trash, "Delete this brush", false),
    ];
    let mut request = None;
    for (n, (icon, tip, always)) in marks.into_iter().enumerate() {
        let live = user || always;
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
            if live { Sense::click() } else { Sense::hover() },
        );
        let colour = match (live, response.hovered()) {
            (false, _) => p.text_dim.gamma_multiply(0.35),
            (true, true) => p.text_strong,
            (true, false) => p.text_dim,
        };
        icons::draw(ui.painter(), hit.shrink(2.0), icon, colour);
        if response
            .on_hover_text(if live { tip } else { READ_ONLY })
            .clicked()
        {
            request = Some(match n {
                0 => Request::Edit(id.to_owned()),
                1 => Request::Rename(id.to_owned()),
                _ => Request::Confirm(id.to_owned()),
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

/// What the brush editor says between its tab strip and its sections: whatever
/// just happened, and the "Save this brush as" field while it is up.
///
/// **Above the body rather than in the footer, and that is the whole reason it
/// is a separate function.** Both of these come and go — a notice is a sentence
/// as long as the import that produced it, and the field is a framed box with a
/// label, a text field and two buttons — so a footer holding them is a footer
/// whose height changes, and the dialog is one size. Drawn here they cost the
/// *body* its space instead, which is what a scroll area is for. It is also
/// where the Brushes panel puts its own notice, so the two read alike.
pub fn save_bar(ui: &mut Ui, p: &Palette, ed: &mut Editor) {
    let mut state = load(ui.ctx(), ed);

    // The browser owns the notice while it is up, so the same sentence is never
    // reported in two places at once.
    if !state.browser_open
        && state.saving.is_none()
        && let Some(notice) = state.notice.clone()
        && notice_bar(ui, p, &notice, true)
    {
        state.notice = None;
    }
    if save_field(ui, p, ed, &mut state) {
        ui.add_space(2.0);
    }

    store(ui.ctx(), state);
}

/// The design's "Save brush" footer, for the brush editor dialog.
///
/// The design draws Cancel and Save at the foot of the editor. Umber's editor
/// applies every change live — deliberately: a paint app should show a change
/// as you make it — so there is nothing for Cancel to undo and it is not drawn.
/// What is left is the half that does something: name what you have made, or
/// write it back over the brush you started from.
///
/// **Its height must not depend on what is in it**, because the dialog reserves
/// exactly `ui::BRUSH_EDITOR_FOOTER` for it and hands the rest to the body. The
/// two things that used to vary are [`save_bar`]'s now; what is left is one
/// line of buttons, and while the field is up it is the same line with the note
/// alone on it — the buttons that arm it would be a second way to start what is
/// already started.
pub fn save_row(ui: &mut Ui, p: &Palette, ed: &mut Editor) {
    let mut state = load(ui.ctx(), ed);

    let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(line, 0.0, p.border);
    ui.add_space(10.0);

    let naming = state.saving.is_some();

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
        // The row is `controls::text_button`'s own 22 points whatever is on it.
        // `ui::BRUSH_EDITOR_FOOTER` reserves exactly what this row costs, and
        // the note alone is only 13 — so without this the footer would lift 9
        // points off the bottom of the dialog the moment the name field went
        // up, which is the same wandering the dialog's fixed size exists to
        // stop, made small enough to be mystifying.
        ui.allocate_exact_size(vec2(0.0, 22.0), Sense::hover());
        controls::note(
            ui,
            p,
            "Changes here reach the brush in your hand straight away. \
             Saving is what keeps them.",
        );
        if naming {
            // The field above is already asking for the name; a button that
            // arms it would be a second way to start what is started.
            return;
        }
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
                    state.saving = Some(Field::new(suggested_name(ed)));
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
                    state.saving = Some(Field::new(suggested_name(ed)));
                }
            }
        });
    });

    store(ui.ctx(), state);
}

/// The name field, when a save has been armed. Returns whether it was drawn, so
/// the caller can leave out the buttons that armed it.
fn save_field(ui: &mut Ui, p: &Palette, ed: &mut Editor, state: &mut State) -> bool {
    let Some(field) = &mut state.saving else {
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
    } else if let Some(field) = &mut state.saving {
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

/// What a save should do about the tip in the brush's hand.
///
/// Two answers, because `BrushPreset::tip` is a **name** and the mask may or
/// may not already be under one:
///
/// - a mask the editor holds a *name* for is already in the library's `tips/`
///   directory (or in the shipped table), so only the name travels — which is
///   the whole reason the field is a name: two brushes cut from one stamp share
///   one file and one GPU upload.
/// - a mask with **no** name has just been imported or drawn and is nowhere on
///   disk yet, so the picture itself has to travel and `UserLibrary::save`
///   writes it out. That is the one path that stores a tip, and there is
///   deliberately no second one.
///
/// A name whose mask this machine does not have still travels as a name. The
/// brush paints round here — `BrushPreset::tip` says so — but taking the
/// reference off would break the brush everywhere else, which is a far worse
/// answer to a missing file.
fn tip_for_save(ed: &Editor) -> (Option<String>, Option<TipMask>) {
    match (&ed.tip, &ed.tip_name) {
        (Some(mask), None) => (None, Some(mask.as_ref().clone())),
        (_, name) => (name.clone(), None),
    }
}

/// Learn the name `UserLibrary::save` gave a mask it has just stored.
///
/// Without this the editor would still be holding a nameless mask, and the next
/// Update would write a second copy of the same picture into `tips/` — every
/// time, for as long as the brush editor stayed open.
fn adopt_saved_tip(ed: &mut Editor, id: &str) -> Option<String> {
    let saved = ed
        .presets
        .iter()
        .find(|preset| preset.id == id)?
        .tip
        .clone();
    ed.tip_name = saved.clone();
    saved
}

fn save_new(state: &mut State, ed: &mut Editor, name: String) {
    let brush = ed.brush;
    let (named, tip) = tip_for_save(ed);
    let label = name.clone();
    let saved = write(state, ed, move |library| {
        library.save(
            BrushPreset {
                tip: named,
                ..BrushPreset::unsaved(name, brush)
            },
            tip,
        )
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
    // The tip is part of what "these settings" means, and all three cases go
    // through here now: taking one off, putting a different one from the
    // library on, and storing one that was imported or drawn since the last
    // save. See [`tip_for_save`].
    let before = preset.tip.clone();
    let (named, mask) = tip_for_save(ed);
    preset.tip = named;
    let label = preset.name.clone();
    if write(state, ed, move |library| library.save(preset, mask)).is_some() {
        let after = adopt_saved_tip(ed, &id);
        state.notice = Some(Notice::good(match (before.as_deref(), after.as_deref()) {
            (Some(_), None) => format!("Updated \"{label}\", and took its bitmap tip off."),
            (None, Some(_)) => format!("Updated \"{label}\", and gave it its bitmap tip."),
            (a, b) if a != b => format!("Updated \"{label}\", and changed its bitmap tip."),
            _ => format!("Updated \"{label}\"."),
        }));
    }
}

// ---------------------------------------------------------------------------
// Making a brush from nothing
// ---------------------------------------------------------------------------

/// What a brush made from nothing is called before anybody renames it.
const NEW_BRUSH: &str = "My brush";

/// Make a brush, put it in the user's hand, and open the editor on it.
///
/// The distinction against the `＋` in the Brushes header is worth stating,
/// because they look alike and are not: `＋` **saves what you are holding**, so
/// it starts from whichever preset was selected and asks for a name. This one
/// starts from [`BrushPreset::fresh`] — the middle of every range, no stamp —
/// which is what somebody who wants to *build* a brush rather than vary one is
/// asking for, and it needs no name field because it can always pick a free
/// one itself.
///
/// Three things here are the ones to be careful of, and all three are
/// `Editor::presets`' doing:
///
/// - The name is uniqued against the **merged** list, not against the user's
///   library, so a new brush is never a second row called what a shipped one is
///   called. `preset::unique_name` is where the rule lives.
/// - The brush is written *before* it is selected, because `write` rebuilds the
///   merged list and every index into it — `Editor::apply_preset` takes an
///   index, so selecting first would select whatever ended up in that slot.
/// - The selection is therefore re-found **by id**, which is the same reason
///   [`resync`] re-finds it that way.
fn new_brush(state: &mut State, ed: &mut Editor) {
    let taken: Vec<String> = ed.presets.iter().map(|p| p.name.clone()).collect();
    let name = preset::unique_name(NEW_BRUSH, NEW_BRUSH, taken.iter().map(String::as_str));
    let label = name.clone();
    let Some(id) = write(state, ed, move |library| {
        library.save(BrushPreset::fresh(name), None)
    }) else {
        return;
    };

    if let Some(index) = ed.presets.iter().position(|preset| preset.id == id) {
        ed.apply_preset(index);
    }
    // Straight into the editor, on the section that decides what the brush *is*
    // — a new brush is an invitation to change something, and landing on
    // whichever tab was last open would hide the answer to "what did that do?".
    ed.ui.brush_editor_open = true;
    ed.ui.brush_tab = BrushTab::Tip;
    // Anything half-armed belongs to the brush that was in hand a moment ago.
    state.saving = None;
    state.renaming = None;
    state.confirming = None;
    // And show it: a new row inside whichever collection happened to be
    // filtered would be a brush somebody has to go and find.
    state.scope = Scope::All;
    state.query.clear();
    state.notice = Some(Notice::good(format!(
        "Made \"{label}\" and put it in your hand. It is saved in your library — \
         change it here, then press Update."
    )));
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
// The tip
// ---------------------------------------------------------------------------

/// The brush editor's Tip section, top row: which stamp this brush uses, and
/// the three ways to change it. Returns whether the brush is stamped, which is
/// what tells the section's Hardness slider it has nothing left to shape.
///
/// Drawn here rather than in `ui.rs` for [`save_row`]'s reason: everything it
/// offers is a reach into the user's library, and the library is this module's.
/// `ui.rs` paints the sliders.
///
/// **`BrushPreset::tip` is a name and this control chooses names.** Picking one
/// out of the list sets `Editor::tip_name` as well as `Editor::tip`, so the
/// mask stays shared with whatever else names it rather than being copied into
/// a second file the moment somebody presses Update — see [`tip_for_save`]. The
/// one entry that is not a name is a picture just imported, which has no name
/// until it is saved.
///
/// Nothing here writes to the library. Every other control in the editor edits
/// the brush in hand and waits for Update, and a tip that persisted on the spot
/// would be the one setting that behaved differently — worse, it would be the
/// one that could not be undone by walking away.
pub fn tip_row(ui: &mut Ui, p: &Palette, ed: &mut Editor) -> bool {
    let mut state = load(ui.ctx(), ed);
    let mut action = None;

    Frame::NONE
        .fill(p.window)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(metrics::RADIUS)
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                tip_preview(ui, p, ed.tip.as_ref());
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    let (title, detail) = tip_labels(ed);
                    ui.label(RichText::new(title).size(text::SMALL).color(p.text_strong));
                    ui.label(RichText::new(detail).size(text::TINY).color(p.text_dim));
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        if link_wide(ui, p, Icon::Import, "Import a picture…", 118.0)
                            .on_hover_text(
                                "Read a PNG, JPEG, TIFF, GIF or BMP as this brush's stamp",
                            )
                            .clicked()
                        {
                            action = Some(TipAction::Import);
                        }
                        if link_wide(ui, p, Icon::Pencil, "Draw a tip…", 96.0)
                            .on_hover_text(
                                "Open a small transparent canvas and paint the stamp yourself",
                            )
                            .clicked()
                        {
                            action = Some(TipAction::Draw);
                        }
                    });
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if let Some(chosen) = tip_picker(ui, p, ed) {
                        action = Some(TipAction::Choose(chosen));
                    }
                });
            });
        });

    match action {
        Some(TipAction::Choose(None)) => ed.clear_tip(),
        Some(TipAction::Choose(Some(name))) => choose_tip(&mut state, ed, &name),
        Some(TipAction::Import) => import_tip(&mut state, ed),
        // Handed back to whoever is drawing the editor: opening a document is
        // `app.rs`'s, because it needs GPU storage for the new canvas.
        Some(TipAction::Draw) => ask_for_tip_canvas(ui.ctx()),
        None => {}
    }

    let stamped = ed.tip.is_some();
    store(ui.ctx(), state);
    stamped
}

/// What [`tip_row`] was asked to do. One per frame: each of them rewrites the
/// state the row was drawn from.
enum TipAction {
    /// A name out of the list, or `None` for the procedural round dab.
    Choose(Option<String>),
    Import,
    Draw,
}

/// Where the Tip section's request for a canvas is left between being made and
/// being carried out.
///
/// A slot of its own rather than a field on [`State`], and that is a cost
/// decision: [`take_draw_request`] is read on **every** frame by `ui::draw`,
/// and `State` is loaded and stored by cloning — a whole `Arc<UserLibrary>`,
/// the index and half a dozen `String`s — which is a price worth paying to draw
/// a list and not worth paying to find out that nobody pressed anything. This
/// is one `bool`.
fn draw_request_id() -> Id {
    Id::new("brush-tip-draw-request")
}

fn ask_for_tip_canvas(ctx: &egui::Context) {
    ctx.data_mut(|d| d.insert_temp(draw_request_id(), true));
}

/// Whether the Tip section asked for a tip document, taken so it is asked for
/// once.
///
/// A flag left in egui's store rather than a return value, because the row is
/// drawn deep inside the brush editor's modal and the caller that can act on it
/// — `ui::draw`, on its way to `UiActions` — is several layers of layout above.
pub fn take_draw_request(ctx: &egui::Context) -> bool {
    ctx.data_mut(|d| d.remove_temp::<bool>(draw_request_id()).unwrap_or(false))
}

/// The heading and the line under it: what this brush stamps.
fn tip_labels(ed: &Editor) -> (&'static str, String) {
    match (&ed.tip, &ed.tip_name) {
        (Some(mask), _) => (
            "Bitmap tip",
            format!("{} × {} px", mask.width(), mask.height()),
        ),
        // A name with no mask. Said out loud rather than drawn as a round
        // brush, because the brush *is* painting round and the reason is worth
        // more than the symptom: the library was copied without its pictures.
        (None, Some(name)) => (
            "Bitmap tip missing",
            format!("\"{name}\" is not in your library — painting round"),
        ),
        (None, None) => (
            "Round tip",
            "The procedural dab, shaped by Hardness".to_owned(),
        ),
    }
}

/// The list of stamps: none, then every mask in the user's library, then
/// whatever this brush names if that is somewhere else.
///
/// Returns the choice, with `None` inside the `Some` meaning "no stamp".
///
/// Deliberately **not** a list of the shipped masks. There are twenty of them,
/// they belong to the brushes they were drawn for, and a picker of twenty
/// names nobody chose would bury the two or three the user made. A shipped one
/// still appears while it is the brush's own tip, so switching away from it and
/// back is possible without losing it.
fn tip_picker(ui: &mut Ui, p: &Palette, ed: &Editor) -> Option<Option<String>> {
    let current = ed.tip_name.as_deref();
    let label = current.unwrap_or("Round");
    let mut chosen = None;
    widgets::dropdown(
        ui,
        p,
        widgets::Dropdown::new(label).width(widgets::DropdownWidth::Exact(132.0)),
        |ui| {
            if ui
                .selectable_label(current.is_none(), "Round — no stamp")
                .clicked()
            {
                chosen = Some(None);
            }
            // A mask in hand that is not in the library yet: imported or drawn,
            // and not stored until the brush is saved. Named as what it is
            // rather than left out, or the picker would show "Round" for a
            // brush that is visibly stamping.
            if current.is_none() && ed.tip.is_some() {
                ui.separator();
                let _ = ui.selectable_label(true, "Not saved yet");
            }
            if !ed.tips.is_empty() {
                ui.separator();
            }
            for name in ed.tips.keys() {
                if ui
                    .selectable_label(current == Some(name.as_str()), name)
                    .clicked()
                {
                    chosen = Some(Some(name.clone()));
                }
            }
            // The brush's own tip, when it came from the shipped table or from
            // a library this machine does not have.
            if let Some(name) = current.filter(|name| !ed.tips.contains_key(*name)) {
                ui.separator();
                let _ = ui.selectable_label(true, name);
            }
        },
    );
    chosen
}

/// Put the named mask in the brush's hand.
///
/// The two-step is `Editor::apply_preset`'s: the user's library first, then the
/// masks Umber ships. A name that resolves to neither still goes on the brush —
/// see `Editor::tip_name` — and the brush paints round until the picture turns
/// up, which is exactly what `BrushPreset::tip` promises.
fn choose_tip(state: &mut State, ed: &mut Editor, name: &str) {
    match ed
        .tips
        .get(name)
        .cloned()
        .or_else(|| umber_core::tip::builtin(name).cloned())
    {
        Some(mask) => {
            ed.set_tip(mask, Some(name.to_owned()));
            state.notice = None;
        }
        None => {
            state.notice = Some(Notice::bad(format!(
                "\"{name}\" is not in your brush library, so this brush paints round."
            )));
        }
    }
}

/// Read a picture off disk as this brush's stamp.
///
/// The mask goes into the brush's hand and **not** into the library: nothing is
/// written until the brush is saved, and `UserLibrary::save` is what stores it
/// in `tips/`. Writing it here would mean a picture in the directory that no
/// preset names, which the library's own `prune_tips` would delete on the next
/// write — so the file would appear and vanish depending on what the user did
/// next.
///
/// Blocking, like every other file dialog in this module.
fn import_tip(state: &mut State, ed: &mut Editor) {
    let Some(path) = rfd::FileDialog::new()
        .set_title("Choose a picture for the brush tip")
        .add_filter(
            "Pictures",
            &["png", "jpg", "jpeg", "tif", "tiff", "gif", "bmp"],
        )
        .add_filter("All files", &["*"])
        .pick_file()
    else {
        return;
    };

    let read = std::fs::read(&path)
        .map_err(|e| format!("{}: {e}", path.display()))
        .and_then(|bytes| TipMask::from_picture(&bytes).map_err(|e| e.to_string()));
    match read {
        Ok((mask, reading)) => {
            let (w, h) = (mask.width(), mask.height());
            // No name: it is nowhere on disk yet. See `tip_for_save`.
            ed.set_tip(Arc::new(mask), None);
            // Which reading was taken is said every time, because it is a guess
            // — the only one in the tip path — and a stamp that came out
            // inverted is otherwise a mystery. See `umber_core::TipReading`.
            state.notice = Some(Notice::good(format!(
                "Took the tip from {} ({w} × {h}) by reading {}. \
                 Save the brush to keep it.",
                file_label(&path),
                reading.describe(),
            )));
        }
        Err(why) => state.notice = Some(Notice::bad(why)),
    }
}

/// Widest a mask is downsampled to for the editor's 48-point thumbnail.
///
/// A stamp can be 2048 texels across, so it is box-averaged down first —
/// nearest sampling would show a sparse spatter tip as an empty square about
/// half the time.
const TIP_PREVIEW_TEXELS: u32 = 96;

/// The 48-point square at the left of [`tip_row`]: the mask, or a round dab
/// where there is none.
fn tip_preview(ui: &mut Ui, p: &Palette, mask: Option<&Arc<TipMask>>) {
    let (rect, _) = ui.allocate_exact_size(vec2(48.0, 48.0), Sense::hover());
    ui.painter().rect_filled(rect, metrics::RADIUS, p.chrome);

    let Some(mask) = mask else {
        // A soft disc: the procedural dab, which is what "no stamp" paints.
        // Drawn rather than left blank, because an empty square reads as a
        // thumbnail that failed to load.
        ui.painter()
            .circle_filled(rect.center(), 15.0, p.text_dim.gamma_multiply(0.5));
        ui.painter().circle_filled(rect.center(), 9.0, p.text_dim);
        return;
    };

    // Kept in egui's temporary store and compared by `Arc` identity, so
    // switching brush rebuilds it and holding the editor open does not. The
    // naive version uploads a texture on every one of the modal's frames.
    let id = Id::new("brush-tip-preview");
    let cached: Option<(Arc<TipMask>, egui::TextureHandle)> = ui.ctx().data(|d| d.get_temp(id));
    let texture = match cached {
        Some((held, texture)) if Arc::ptr_eq(&held, mask) => texture,
        _ => {
            let texture = ui.ctx().load_texture(
                "brush-tip",
                widgets::tip_image(mask, p.text_strong, TIP_PREVIEW_TEXELS),
                egui::TextureOptions::LINEAR,
            );
            ui.ctx()
                .data_mut(|d| d.insert_temp(id, (Arc::clone(mask), texture.clone())));
            texture
        }
    };
    ui.painter().image(
        texture.id(),
        rect.shrink(2.0),
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
}

// ---------------------------------------------------------------------------
// Drawing a tip on the canvas
// ---------------------------------------------------------------------------

/// The strip along the top of a document that is a brush stamp rather than a
/// picture. Returns whether "Use as tip" was pressed.
///
/// The same shape as the layout-edit bar and for the same reason: a mode you
/// are in has to say so, because everything else about the window looks
/// exactly as it did. Here it matters more than there — the canvas is 256
/// pixels of nothing, and without the strip the only clue is the tab's name.
///
/// Nothing is drawn on an ordinary document, which is every document but this
/// one.
pub fn tip_bar(root: &mut Ui, p: &Palette, ed: &mut Editor) -> bool {
    let Some(target) = ed.session.active_tab().tip_for.clone() else {
        return false;
    };
    // Whether the brush it names is still there. A brush deleted while its tip
    // canvas was open is a real state and must not be a surprise at the end —
    // it is said here, before the work is done, and "Use as tip" still works:
    // the stamp goes into the brush in hand instead. See `app`'s `commit_tip`.
    let gone = !ed.presets.iter().any(|preset| preset.id == target.brush);

    let mut commit = false;
    let frame = Frame {
        fill: p.control_active,
        stroke: Stroke::new(1.0, p.accent_dim),
        inner_margin: Margin::symmetric(12, 0),
        ..Default::default()
    };
    egui::Panel::top("tip-document-bar")
        .exact_size(metrics::EDIT_BAR)
        .frame(frame)
        .show(root, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(
                    RichText::new("BRUSH TIP")
                        .size(text::TINY)
                        .color(p.accent)
                        .strong(),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(if gone {
                        format!(
                            "for \"{}\", which is no longer in your library · \
                             what you paint becomes coverage — colour is ignored, \
                             opacity is the strength",
                            target.name
                        )
                    } else {
                        format!(
                            "for \"{}\" · what you paint becomes coverage — colour is \
                             ignored, opacity is the strength, and the eraser takes it \
                             back off",
                            target.name
                        )
                    })
                    .size(text::TINY)
                    .color(p.text_dim),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if controls::text_button(ui, p, "Use as tip", true, true)
                        .on_hover_text(if gone {
                            "Put the stamp in the brush you are holding — the brush this \
                             canvas was for has gone"
                        } else {
                            "Write this canvas into your brush library as that brush's stamp"
                        })
                        .clicked()
                    {
                        commit = true;
                    }
                });
            });
        });
    commit
}

/// What became of turning a tip canvas into a stamp, phrased for the user.
///
/// Sentences rather than an enum the caller has to word, because two of the
/// three are not failures and not successes either — the interesting one is
/// "the brush has gone, so the stamp is in your hand", which has to say both
/// halves or it reads as a loss.
pub struct TipCommit {
    pub title: String,
    pub detail: String,
}

/// Put a finished tip canvas onto the brush it was drawn for.
///
/// The caller supplies the mask, because reading the canvas back is the GPU's;
/// what is decided here is where it goes, and there are only two answers:
///
/// - the brush is in the **user's** library, so the stamp is written onto it
///   through `UserLibrary::save` — the one path that stores a tip, the same one
///   an import and a Save use.
/// - it is not, because it has been deleted since the canvas was opened or
///   because it is a shipped brush that cannot be written to. Then the stamp
///   goes into the brush in hand, unnamed, and "Save as new…" is what keeps it.
///   Nothing is lost either way, and neither answer can panic: the id is looked
///   up, never indexed.
///
/// The mask is put in the editor's hand in **both** cases, so that the very
/// next stroke uses it. That is also what makes the write visible: a tip you
/// have just drawn and cannot see the effect of reads as nothing having
/// happened.
pub fn commit_tip(
    ctx: &egui::Context,
    ed: &mut Editor,
    target: &umber_core::TipTarget,
    mask: TipMask,
) -> TipCommit {
    let mut state = load(ctx, ed);
    let owned =
        matches!(&state.store, Store::Ready(library) if library.get(&target.brush).is_some());

    let outcome = if owned {
        let id = target.brush.clone();
        let stored = mask.clone();
        let saved = write(&mut state, ed, move |library| {
            let mut preset = library.get(&id).cloned().ok_or_else(|| {
                PresetError::Malformed(None, "that brush is no longer in your library".to_owned())
            })?;
            // `None` for the name and the mask alongside is what stores it:
            // `UserLibrary::save` writes the picture into `tips/` and puts the
            // name it chose on the preset.
            preset.tip = None;
            library.save(preset, Some(stored))
        });
        match saved {
            Some(id) => {
                // Selected rather than merely written, so the brush in hand is
                // the one that now has the stamp — and `apply_preset` is what
                // resolves the name the library just allocated.
                if let Some(index) = ed.presets.iter().position(|preset| preset.id == id) {
                    ed.apply_preset(index);
                }
                TipCommit {
                    title: format!("\"{}\" now stamps what you drew", target.name),
                    detail: "The picture is in your brush library's tips folder, and the \
                             brush is in your hand. This canvas is still open — paint on \
                             it again and press Use as tip to replace the stamp."
                        .to_owned(),
                }
            }
            // `write` has already put the reason in the library's own notice;
            // it is lifted out here so the modal can carry the same sentence
            // rather than a second, vaguer one.
            None => TipCommit {
                title: "The tip could not be saved".to_owned(),
                detail: state
                    .notice
                    .as_ref()
                    .map_or_else(String::new, |notice| notice.text.clone()),
            },
        }
    } else {
        ed.set_tip(Arc::new(mask), None);
        TipCommit {
            title: format!("The stamp is in your hand, not on \"{}\"", target.name),
            detail: format!(
                "\"{}\" is not a brush Umber can write to — it has been deleted, or it is \
                 one Umber ships and those are read-only. Nothing is lost: you are \
                 painting with the stamp now, and \"Save as new…\" in the brush editor \
                 keeps it.",
                target.name
            ),
        }
    };

    store(ctx, state);
    outcome
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
                    |ui| browser_rail(ui, p, ed, state),
                );
                ui.allocate_ui_with_layout(
                    vec2(width - metrics::BRUSH_LIBRARY_RAIL, height),
                    Layout::top_down(Align::Min),
                    |ui| browser_pane(ui, p, ed, state),
                );
            });
        });

    if let Some(drag) = &state.drag {
        drag_ghost(root.ctx(), p, drag);
    }

    if response.should_close() {
        close_browser(state);
    }
}

/// The label that follows the pointer while a brush is being carried.
///
/// On egui's tooltip layer, so it rides over the modal and over the rail
/// instead of being clipped to the list it came out of. Painted rather than
/// added as a widget: nothing about it is interactive, and a widget sitting
/// under the pointer through a drag would take the hover the rail's rows need.
///
/// It names the destination as well as the brush, because "let go here" is the
/// one thing a drag has to be able to answer before it is finished — and where
/// it says nothing, letting go does nothing.
fn drag_ghost(ctx: &egui::Context, p: &Palette, drag: &brushdrag::Drag) {
    let Some(pointer) = ctx.input(|i| i.pointer.interact_pos()) else {
        return;
    };
    ctx.set_cursor_icon(egui::CursorIcon::Grabbing);

    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        Id::new("brush-library-drag"),
    ));
    // An em dash rather than an arrow. Archivo carries no arrow glyph and would
    // draw a blank box; the dash is ordinary punctuation and is already how the
    // attribution tooltip joins two things together.
    let label = match drag.destination() {
        Some(to) => format!("{} — {to}", drag.name),
        None => drag.name.clone(),
    };
    let galley = painter.layout_no_wrap(label, FontId::proportional(text::TINY), p.text_strong);
    let rect = Rect::from_min_size(pointer + vec2(14.0, 12.0), galley.size() + vec2(16.0, 9.0));
    painter.rect_filled(rect, metrics::RADIUS, p.popover);
    painter.rect_stroke(
        rect,
        metrics::RADIUS,
        Stroke::new(1.0, p.popover_border),
        egui::StrokeKind::Inside,
    );
    painter.galley(rect.min + vec2(8.0, 4.5), galley, p.text_strong);
}

fn close_browser(state: &mut State) {
    state.browser_open = false;
    state.renaming = None;
    state.confirming = None;
    // The rail is the only place a collection can be made, so a field left
    // armed would come back over a dialog the user has since reopened for
    // something else.
    state.creating = None;
    // The rail goes with the browser, so a drag that outlived it would be a
    // brush being carried towards targets that are no longer on screen.
    state.drag = None;
}

/// The field the rail's `＋` arms: name a collection, or back out of it.
///
/// Drawn nowhere unless a collection is being made, and shaped like the "Save
/// this brush as" field for the same reason — one way of asking for a name in
/// this module rather than two.
///
/// Nothing here touches [`crate::shortcuts`]. Dispatch is suspended for the
/// whole interface by `ui::draw`, from `text_edit_focused`, which is
/// deliberately one lever rather than one per field: a module that pulls it for
/// its own fields only ever covers the fields it knows about.
fn new_collection_field(ui: &mut Ui, p: &Palette, ed: &mut Editor, state: &mut State) {
    let Some(field) = &mut state.creating else {
        return;
    };
    let mut name = std::mem::take(&mut field.text);
    let focus = std::mem::take(&mut field.focus);
    let mut commit = false;
    let mut cancel = false;

    Frame::NONE
        .fill(p.window)
        .stroke(Stroke::new(1.0, p.accent_dim))
        .corner_radius(metrics::RADIUS)
        .inner_margin(Margin::symmetric(7, 6))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 5.0;
            ui.label(
                RichText::new("New collection")
                    .size(text::TINY)
                    .color(p.text_dim),
            );
            let edit = ui.add(
                TextEdit::singleline(&mut name)
                    .id(Id::new("brushlib-new-collection"))
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
                    if controls::text_button(ui, p, "Create", true, named)
                        .on_hover_text(if named {
                            "Add it to the rail, ready for brushes to be dragged onto it"
                        } else {
                            "Give the collection a name first."
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
        state.creating = None;
    } else if commit && !name.trim().is_empty() {
        // Put the typed name back before trying, so a write that fails leaves
        // the field holding what the user typed rather than empty.
        if let Some(field) = &mut state.creating {
            field.text = name.clone();
        }
        create_collection(state, ed, name.trim().to_owned());
    } else if let Some(field) = &mut state.creating {
        field.text = name;
    }
}

/// Make the collection the field names, and say what came of it.
///
/// The clash rules are `UserLibrary::create_collection`'s, not this function's,
/// which is what lets them be tested without a window. What this side supplies
/// is the one thing the model cannot work out: the collections already on the
/// rail. They are *derived* from the merged preset list, and the shipped half
/// of that list lives in the binary — see `preset::Library::collections`.
fn create_collection(state: &mut State, ed: &mut Editor, name: String) {
    // Cloned rather than borrowed out of the index: `write` takes the state
    // mutably. Once per press of Create, not per frame.
    let existing: Vec<String> = state
        .index
        .groups
        .iter()
        .map(|group| group.name.clone())
        .collect();
    let asked = name.clone();
    let Some(outcome) = write(state, ed, move |library| {
        library.create_collection(&name, &existing)
    }) else {
        return;
    };
    match outcome {
        NewCollection::Created => {
            state.creating = None;
            // Show it. A row appearing somewhere in a rail of fifteen is easy
            // to miss, and an empty collection nobody is looking at reads as
            // nothing having happened.
            state.scope = Scope::Category(asked.clone());
            state.query.clear();
            state.notice = Some(Notice::good(format!(
                "Made \"{asked}\". Drag brushes onto it to file them there."
            )));
        }
        NewCollection::Exists => {
            state.creating = None;
            // The collection asked for is already there, so the useful answer
            // is to go to it rather than to make a second row of the same name.
            // Matched case-insensitively, like the refusal, and shown under the
            // spelling the rail already uses.
            let found = state
                .index
                .groups
                .iter()
                .find(|group| preset::same_collection(&group.name, &asked))
                .map(|group| group.name.clone());
            state.notice = Some(Notice::bad(match &found {
                Some(name) => {
                    format!("\"{name}\" is already a collection — showing that one instead.")
                }
                // Only reachable for a collection that exists as a rule rather
                // than as a row: `preset::IMPORTED` is where every import
                // lands, whether or not anything has been imported yet.
                None => format!(
                    "\"{asked}\" is where Umber files imported brushes, so it cannot be made by hand."
                ),
            }));
            if let Some(name) = found {
                state.scope = Scope::Category(name);
                state.query.clear();
            }
        }
        // The Create button is dead until the field has something in it, so
        // this is only reachable by a stray Enter. Leave the field up.
        NewCollection::Unnamed => {}
    }
}

fn browser_rail(ui: &mut Ui, p: &Palette, ed: &mut Editor, state: &mut State) {
    let writable = state.writable();
    let why_not = state.why_not().to_owned();
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
                // The same `＋` the Brushes panel's header carries, and here for
                // the same reason: every other collection on this rail is
                // derived from a brush that is already in one, so without it
                // there is no way to make the empty collection a brush would be
                // dragged into first.
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(
                        ui,
                        p,
                        Icon::Plus,
                        writable,
                        if writable {
                            "Make a new collection"
                        } else {
                            why_not.as_str()
                        },
                    ) {
                        state.creating = Some(Field::new(String::new()));
                        state.notice = None;
                    }
                });
            });
            ui.add_space(12.0);

            new_collection_field(ui, p, ed, state);

            let mut chosen = None;
            let all = format!("All brushes ({})", state.index.total);
            if controls::sidebar_tab(ui, p, &all, state.scope == Scope::All, true, "").clicked() {
                chosen = Some(Scope::All);
            }
            ui.add_space(8.0);

            // Where every collection row landed, so a brush dragged out of the
            // list can be dropped on one. Collected only while something is
            // actually being carried: the rail is redrawn every frame and this
            // would otherwise be a `Vec` of fourteen names built sixty times a
            // second to answer a question nobody is asking.
            let dragging = state.drag.is_some();
            let mut rows: Vec<brushdrag::Row> = Vec::new();

            // The row a drop would land on, as [`brushdrag::Drag::aim`] left it
            // at the end of the *last* frame. One frame behind the pointer,
            // which nobody can see in a drag, and what it buys is that the mark
            // can be painted as part of the row rather than over the top of it
            // — `sidebar_tab` draws its own label, and a highlight laid on
            // afterwards would cover the name of the collection it is pointing
            // at.
            let aimed = state.drag.as_ref().and_then(brushdrag::Drag::destination);

            egui::ScrollArea::vertical()
                .id_salt("brush-library-collections")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for group in &state.index.groups {
                        let scope = Scope::Category(group.name.clone());
                        let label = format!("{} ({})", group.name, group.members.len());
                        let target = aimed == Some(group.name.as_str());
                        let response = controls::sidebar_tab(
                            ui,
                            p,
                            &label,
                            state.scope == scope || target,
                            true,
                            "",
                        );
                        if target {
                            // An outline as well as the fill, so "the brush
                            // lands here" cannot be read as "this collection is
                            // the one being shown".
                            ui.painter().rect_stroke(
                                response.rect,
                                metrics::RADIUS,
                                Stroke::new(1.0, p.accent),
                                egui::StrokeKind::Inside,
                            );
                        }
                        if dragging {
                            rows.push(brushdrag::Row {
                                name: group.name.clone(),
                                rect: response.rect,
                            });
                        }
                        // A release over the rail ends a drag; it is not also a
                        // click on whichever collection it landed on.
                        if response.clicked() && !dragging {
                            chosen = Some(scope);
                        }
                    }
                });

            // Take this frame's aim, for the drop and for the mark above.
            if let Some(drag) = &mut state.drag {
                let pointer = ui.input(|i| i.pointer.interact_pos());
                drag.aim(&rows, pointer);
            }

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
                    // Right-to-left, so New brush lands left of Import — the
                    // same pairing and the same order the panel's links keep.
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
                    if controls::text_button(ui, p, "New brush", false, writable)
                        .on_hover_text(if writable {
                            "Make a brush from Umber's defaults and open it in the brush editor"
                        } else {
                            why_not.as_str()
                        })
                        .clicked()
                    {
                        new_brush(state, ed);
                        // The editor it opens is a modal, and two modals over
                        // each other leaves neither reachable.
                        close_browser(state);
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
        Some(Request::Edit(id)) => {
            // Selected first, because the editor edits the brush *in your
            // hand*: there is no second path that edits a preset in place, and
            // there must not be — a change to a shipped brush cannot be written
            // back to it, so what the editor changes is `Editor::brush` and
            // what keeps the change is the footer's Save.
            //
            // By id and not by the row's index, for `resync`'s reason: the
            // merged list is rebuilt under this module and a position does not
            // survive it.
            if let Some(index) = ed.presets.iter().position(|preset| preset.id == id) {
                ed.apply_preset(index);
            }
            ed.ui.brush_editor_open = true;
            // The browser is a modal and so is the editor, and two modals over
            // each other leaves neither reachable — the same reason the
            // browser's own "New brush" closes it.
            close_browser(state);
        }
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
        Some(Request::Grab(id, name, from)) => {
            // A drag is not an edit, so anything half-finished gets out of the
            // way rather than being left open behind the moving row.
            state.renaming = None;
            state.confirming = None;
            state.drag = Some(brushdrag::Drag::new(id, name, from));
        }
        Some(Request::Drop) => {
            if let Some(drag) = state.drag.take() {
                move_to_collection(state, ed, &drag);
            }
        }
        None => {
            if let Some(index) = out.picked {
                ed.apply_preset(index);
            }
        }
    }
}

/// File a dragged brush wherever the rail says it landed.
///
/// A drop that lands on nothing — off the rail, or on the collection the brush
/// is already in — is not a failure and says nothing: the user let go
/// somewhere, and a line of explanation for a gesture they abandoned is noise.
fn move_to_collection(state: &mut State, ed: &mut Editor, drag: &brushdrag::Drag) {
    let Some(to) = drag.destination() else {
        return;
    };
    let (id, to) = (drag.id.clone(), to.to_owned());
    let label = drag.name.clone();
    // `assign` takes the id rather than a position, and takes it for shipped
    // brushes as well as the user's own — see `Library::collections` for where
    // a shipped brush's collection has to live to survive an update.
    if write(state, ed, |library| library.assign(&id, Some(&to))).is_some() {
        state.notice = Some(Notice::good(format!("Moved \"{label}\" to {to}.")));
        // The collection it came out of may have been the last brush in it, and
        // an empty one is not in the rail at all. Showing it would be a list
        // with nothing in it and no way to say why.
        if let Scope::Category(showing) = &state.scope
            && !state
                .index
                .groups
                .iter()
                .any(|group| group.name == *showing)
        {
            state.scope = Scope::All;
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

/// An icon followed by a label, behaving as one clickable unit, at a stated
/// width.
///
/// `ui.rs` has one of these but keeps it private, and that file belongs to the
/// workspace rather than to this feature. This one elides a label that does not
/// fit and draws a disabled state — which the library needs, because a broken
/// library still has to explain itself.
///
/// The width is stated rather than taken from the row, for the two that share a
/// row rather than having one to themselves: an `available_width` there is the
/// whole rest of the row, so the first one drawn would swallow the second.
fn link_wide(ui: &mut Ui, p: &Palette, icon: Icon, label: &str, width: f32) -> Response {
    sized_link(ui, p, icon, label, true, width)
}

fn sized_link(
    ui: &mut Ui,
    p: &Palette,
    icon: Icon,
    label: &str,
    enabled: bool,
    width: f32,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(
        vec2(width, 18.0),
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

/// A state the panel, the browser and the brush editor can all be drawn from
/// without reading the user's own library off the machine the tests run on.
///
/// [`load`] returns whatever is already in the context, so seeding it is what
/// keeps a test from depending on — or, where a pre-tips `brushes.ron` is
/// sitting there, *migrating* — a brush directory belonging to whoever is
/// running it. A broken store rather than an empty one, because there is no way
/// to build a [`UserLibrary`] that is not a directory somewhere: what it costs
/// is that everything which writes draws disabled, which is a state worth
/// laying out anyway.
///
/// `query` is a string no preset matches, so a caller measuring the panel is
/// measuring its furniture rather than this machine's brush collection.
#[cfg(test)]
pub(crate) fn seed_broken_library(ctx: &egui::Context, ed: &Editor, why: &str) {
    store(
        ctx,
        State {
            index: Arc::new(Index::build(&ed.presets, &[])),
            store: Store::Broken(why.to_owned()),
            query: "zzzz".to_owned(),
            scope: Scope::All,
            browser_open: false,
            saving: None,
            renaming: None,
            confirming: None,
            creating: None,
            drag: None,
            notice: None,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing the Brushes panel offers sits below its list.
    ///
    /// The body is one scroll area, so a control under the list is a control a
    /// panel dragged short hides — which is exactly why the design's
    /// `✎ Edit "<name>"…` link became a mark in the header. Two more links
    /// stayed there anyway, "New brush…" and "Import brushes…", and they had
    /// the same fault. New brush is the header's `＋` now and Import is in the
    /// browser.
    ///
    /// Measured rather than asserted about the source, and measured with a
    /// search that matches nothing so that the list is one note and the bound
    /// is about the *furniture*: a broken-library notice, the search field, the
    /// collection picker and that note. That comes to 121 points. A link row is
    /// 18 plus egui's 6 of spacing, so **one** put back under the list is 145
    /// and fails here; the bound is not set at 122, because the same layout has
    /// to hold on three platforms.
    #[test]
    fn the_brushes_panel_body_ends_at_its_list() {
        use crate::editor::Editor;
        use crate::theme::{Palette, ThemeKind};
        use egui::{Rect, pos2};

        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                pos2(0.0, 0.0),
                vec2(metrics::PANEL, 600.0),
            )),
            ..Default::default()
        };
        let palette = Palette::of(ThemeKind::Graphite);
        let mut ed = Editor::default();

        // Twice, and the second is the one read: the first pass through a fresh
        // context builds the font atlas, and text laid out against a half-built
        // one is not the height it will settle at.
        let mut measured = 0.0;
        for _ in 0..2 {
            seed_broken_library(&ctx, &ed, "no library");
            let _ = ctx.run_ui(input.clone(), |ui| {
                super::panel(ui, &palette, &mut ed);
                measured = ui.min_rect().height();
            });
        }

        assert!(
            measured < 135.0,
            "the Brushes panel body is {measured} points with nothing listed, \
             which is room for a control under the list"
        );
    }

    /// The library browser, with a shipped brush and one of the user's own in
    /// the same list.
    ///
    /// Written rather than asserted for the reason `layers_panel_preview` is:
    /// what changed is *which of three marks on a row is alive*, and the only
    /// question worth asking about that is whether somebody can tell the live
    /// pencil from the dead rename and bin beside it — which no assertion about
    /// widgets can answer. The row is also the one place three 18-point marks
    /// share a 40-point row with a name, a sample and a credit line.
    ///
    /// ```sh
    /// cargo test -p umber-app brush_library_browser_preview -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "writes preview PNGs and wants a GPU; run deliberately"]
    #[cfg(debug_assertions)]
    fn brush_library_browser_preview() {
        use crate::dock::Layout;
        use crate::docshot;
        use crate::editor::Editor;
        use crate::theme::Palette;

        let Some(mut stage) = docshot::Stage::new() else {
            eprintln!("no GPU adapter: nothing to draw into. Skipped.");
            return;
        };
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/brush-editor");
        std::fs::create_dir_all(&dir).expect("create the preview directory");

        let mut ed = Editor::default();
        ed.layout = Layout::default();
        // One brush past the shipped library, which is what `Index::is_user`
        // reads, so the list holds both a row whose three marks are all live
        // and rows where only the pencil is.
        // Named to match the search below and filed in "My brushes", which
        // `Index::rank` puts first — so the row where all three marks are live
        // is the top one, directly above the shipped rows where only the pencil
        // is.
        let mut mine = preset("user/mine", "Round, mine", "My brushes");
        mine.collection = Some("My brushes".to_owned());
        ed.presets.push(mine);
        let palette = Palette::with_accent(ed.ui.theme, ed.ui.accent);

        let [w, h] = metrics::BRUSH_LIBRARY;
        let field = vec2(w + 48.0, h + 48.0);
        let image = stage.shoot(field, 1.5, &palette, palette.backdrop, |root| {
            // Re-seeded every frame: `load` would otherwise reach the brush
            // library of whoever is running this, and the browser would open on
            // their collection rather than on Umber's.
            store(
                root.ctx(),
                State {
                    index: Arc::new(Index::build(&ed.presets, &[])),
                    store: Store::Broken("no library".to_owned()),
                    query: "round".to_owned(),
                    scope: Scope::All,
                    browser_open: true,
                    saving: None,
                    renaming: None,
                    confirming: None,
                    creating: None,
                    drag: None,
                    notice: None,
                },
            );
            super::dialogs(root, &palette, &mut ed);
        });
        docshot::write_png(&dir.join("browser.png"), &image).expect("write the png");
        println!("wrote the browser to {}", dir.display());
    }

    fn preset(id: &str, name: &str, category: &str) -> BrushPreset {
        BrushPreset {
            id: id.to_owned(),
            name: name.to_owned(),
            category: category.to_owned(),
            collection: None,
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
        let index = Index::build(&presets, &[]);

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
        // And a *shipped* brush dragged into your collection does not send that
        // collection to the bottom of the rail. It is the name that makes a
        // collection yours, not who happens to be in it.
        let mut with_a_shipped_one = presets.clone();
        with_a_shipped_one[0].collection = Some("My brushes".to_owned());
        assert_eq!(
            Index::build(&with_a_shipped_one, &[]).groups[0].name,
            "My brushes"
        );
        assert_eq!(index.total, presets.len());
        // Every preset lands in exactly one collection, or the picker would
        // quietly hide brushes.
        let members: usize = index.groups.iter().map(|g| g.members.len()).sum();
        assert_eq!(members, presets.len());
    }

    /// An import has to be findable the moment it arrives, which the classifier
    /// cannot manage: it files a pack of twenty across six collections, all of
    /// them already holding two hundred shipped brushes.
    #[test]
    fn an_import_is_filed_under_imported_rather_than_by_its_style() {
        let mut presets = preset::builtin().to_vec();
        let mut arrived = preset("mypaint/charcoal-4", "Charcoal 4", style::Style::CHARCOAL);
        arrived.collection = Some(preset::IMPORTED.to_owned());
        presets.push(arrived);
        let last = presets.len() - 1;
        let index = Index::build(&presets, &[]);

        // Not with the charcoals its name would have put it among…
        let charcoal = index
            .groups
            .iter()
            .find(|g| g.name == style::Style::CHARCOAL)
            .expect("the shipped library has charcoals");
        assert!(!charcoal.members.contains(&last));
        // …but in one collection of its own, at the top of the rail with
        // everything else that is the user's rather than Umber's.
        assert_eq!(index.groups[0].name, preset::IMPORTED);
        assert_eq!(index.groups[0].members, vec![last]);
        // And the search answers to where it is filed, not to what it is.
        assert!(matches(&presets[last], "imported"));
        assert!(!matches(&presets[last], "chalk"));
    }

    /// A collection with nothing in it cannot be derived from the presets, so
    /// it has to reach the rail from the library's own list — and it has to
    /// land where the user's collections land, or the row they just made would
    /// appear below two hundred shipped brushes' worth of styles.
    #[test]
    fn a_collection_the_user_made_is_a_row_of_its_own_with_nothing_in_it() {
        let made = ["Comics".to_owned()];
        let index = Index::build(preset::builtin(), &made);
        let comics = index
            .groups
            .iter()
            .find(|group| group.name == "Comics")
            .expect("the made collection is on the rail");
        assert!(comics.members.is_empty());
        // Yours first: a name `style::classify` could never produce is one
        // somebody chose.
        assert_eq!(index.groups[0].name, "Comics");
        // And it is a row, not a brush: the count in the footer must not move.
        assert_eq!(index.total, preset::builtin().len());
    }

    /// Once a brush is dragged in, the collection is derived *and* recorded.
    /// Two rows of one name would be two places to drop a brush and one place
    /// to look for it afterwards.
    #[test]
    fn a_made_collection_that_has_gained_a_brush_is_still_one_row() {
        let mut presets = preset::builtin().to_vec();
        let mut moved = preset("user/nib", "Nib", "Inks & pens");
        moved.collection = Some("Comics".to_owned());
        presets.push(moved);
        let last = presets.len() - 1;

        let index = Index::build(&presets, &["Comics".to_owned()]);
        let rows: Vec<&Group> = index
            .groups
            .iter()
            .filter(|group| group.name == "Comics")
            .collect();
        assert_eq!(rows.len(), 1, "the collection was listed twice");
        assert_eq!(rows[0].members, vec![last]);

        // The spelling in the file need not match the one the brush carries;
        // the two are still one collection.
        let index = Index::build(&presets, &["comics".to_owned()]);
        assert_eq!(
            index
                .groups
                .iter()
                .filter(|group| preset::same_collection(&group.name, "Comics"))
                .count(),
            1
        );
    }

    /// The one place in the interface that would otherwise report the feature
    /// as broken: a collection made a moment ago is empty because it is new,
    /// and "No brush matches that" reads as a search that failed.
    #[test]
    fn an_empty_collection_says_what_to_do_with_it_rather_than_that_nothing_matched() {
        let mut state = State {
            store: Store::Broken(String::new()),
            index: Arc::new(Index::build(&[], &[])),
            query: String::new(),
            scope: Scope::Category("Comics".to_owned()),
            browser_open: true,
            saving: None,
            renaming: None,
            confirming: None,
            creating: None,
            drag: None,
            notice: None,
        };
        assert!(empty_message(&state).contains("Drag a brush"));
        // A search that genuinely found nothing still says so.
        state.query = "zzz".to_owned();
        assert_eq!(empty_message(&state), "No brush matches that.");
        state.query.clear();
        state.scope = Scope::All;
        assert_eq!(empty_message(&state), "No brush matches that.");
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
        let index = Index::build(preset::builtin(), &[]);
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

    /// `BrushPreset::tip` is a **name**, so a save has to know when it is
    /// writing one down and when it is storing a picture. Getting this the
    /// wrong way round is expensive in both directions: a name sent as a mask
    /// puts a second copy of the same picture in `tips/` on every Update, and a
    /// mask sent as a name is a stamp brush saved as a round one.
    #[test]
    fn a_named_tip_travels_as_a_name_and_a_fresh_one_as_the_picture() {
        let mut ed = Editor::default();
        let mask = Arc::new(TipMask::new(2, 2, vec![255; 4]).expect("mask"));

        // Nothing in hand: nothing to say.
        assert_eq!(tip_for_save(&ed), (None, None));

        // Just imported or drawn — nowhere on disk, so the picture travels.
        ed.set_tip(Arc::clone(&mask), None);
        let (named, picture) = tip_for_save(&ed);
        assert_eq!(named, None);
        assert_eq!(picture.as_ref(), Some(mask.as_ref()));

        // Already in the library: the name travels and the file is shared.
        ed.set_tip(Arc::clone(&mask), Some("user-nib".to_owned()));
        assert_eq!(
            tip_for_save(&ed),
            (Some("user-nib".to_owned()), None),
            "a stored mask must not be written a second time"
        );

        // A name whose picture this machine does not have. The brush paints
        // round here, but taking the reference off would break it everywhere
        // else — see `Editor::tip_name`.
        ed.clear_tip();
        ed.tip_name = Some("gone".to_owned());
        assert_eq!(tip_for_save(&ed), (Some("gone".to_owned()), None));

        // And taking the tip off clears both halves, or an Update would put the
        // reference straight back on.
        ed.set_tip(mask, Some("user-nib".to_owned()));
        ed.clear_tip();
        assert_eq!(tip_for_save(&ed), (None, None));
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
