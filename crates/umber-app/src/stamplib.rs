//! The stamps and papers a brush paints through: browsing them, importing
//! them, and choosing one.
//!
//! `umber_core::tip` is the model — what a mask is, what a picture means as a
//! stamp or as a paper, and whether a tile joins to itself — and
//! `umber_core::preset::UserLibrary` is where they are kept. Nothing in either
//! draws and nothing in this one decides. Same division `dock.rs` keeps against
//! `panels.rs`.
//!
//! The shape is [`crate::brushlib`]'s and [`crate::palettelib`]'s, because it
//! answers the same question, and the arguments carry over one for one. What is
//! its own:
//!
//! - **The store is `brushlib`'s, not a second one.** A stamp and a paper live
//!   in the *brush* library's directory and are written by the same atomic
//!   `brushes.ron` write, so a second `UserLibrary` loaded here would be a
//!   second copy of somebody's collection with two writers. Everything goes
//!   through `brushlib::library` and `brushlib::edit_library`, which is also
//!   what keeps the `resync` after every write from being skipped.
//! - **Shipped and user pictures are one list with the user's first**, which is
//!   the merged-preset rule and the same resolution order `Editor::paper_tile`
//!   and `Editor::apply_preset` use — the user's library first, then what Umber
//!   ships. A name that is in both resolves to the user's, and the list says
//!   which is which rather than silently hiding one.
//! - **A paper is previewed *tiled*.** It is the one thing about a texture that
//!   a single square cannot show and the one thing that ruins a painting: grain
//!   is anchored to the document and wraps across it, so a tile that does not
//!   join draws a grid over the whole canvas. Two by two is enough to put both
//!   joins inside the preview.
//! - **The tip picker in the brush editor still lists names and this lists
//!   pictures**, and both are wanted. That picker deliberately does not offer
//!   the shipped masks — twenty names nobody chose would bury the two the user
//!   made — and the reason that argument holds is exactly that it is a list of
//!   *names*. Here they are pictures with a size beside them, which is what
//!   makes twenty of them worth having.

use std::sync::Arc;

use egui::{Align, Frame, Id, Layout, Margin, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};

use umber_core::TipMask;
use umber_core::preset::Removed;

use crate::brushlib::{self, Notice};
use crate::controls;
use crate::editor::Editor;
use crate::icons::Icon;
use crate::theme::{Palette, metrics, text};
use crate::ui::icon_button;
use crate::widgets;

/// Which half of the library the browser is showing.
///
/// Two lists in one modal rather than two modals, because the question behind
/// both is "which picture" and the answer comes out of one directory — and
/// because a painter comparing a stamp against the paper it will bite through
/// should not have to shut one window to open the other.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Stamps,
    Papers,
}

impl Kind {
    fn title(self) -> &'static str {
        match self {
            Self::Stamps => "Stamps",
            Self::Papers => "Papers",
        }
    }

    /// How many copies of the picture a preview shows across and down.
    ///
    /// Two for a paper, because the grain repeats across the document and a
    /// seam is invisible until the tile meets itself; one for a stamp, which is
    /// stretched over a single dab and has nothing to meet.
    pub(crate) fn repeats(self) -> u32 {
        match self {
            Self::Stamps => 1,
            Self::Papers => 2,
        }
    }

    /// What a row's Use button does, said before it is pressed.
    fn use_tip(self) -> &'static str {
        match self {
            Self::Stamps => "Stamp the brush in hand with this",
            Self::Papers => "Paint the brush in hand through this paper",
        }
    }
}

/// The gap between two rows of the browser's list.
///
/// Named because `show_rows` is told the row height and adds this itself, so
/// the two figures have to be the ones really in force or the scroll position
/// drifts from the rows under it.
const ROW_GAP: f32 = 6.0;

/// Where one picture in the list came from.
///
/// Drawn on the row rather than inferred from the name, because the two
/// namespaces overlap by construction — the user's library is consulted first,
/// so a stamp of theirs called `umber-stipple` hides the shipped one, and a row
/// that did not say so would be a picture nobody could account for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    Yours,
    Shipped,
    /// A shipped picture a user's own has taken the name of. Listed and not
    /// offered: choosing it is not something a name can express.
    Hidden,
}

#[derive(Clone, Default)]
struct State {
    open: Option<Kind>,
    query: String,
    notice: Option<Notice>,
    /// The name whose Remove has been pressed once. Removing a picture cannot
    /// be undone — the history covers painting only — so it asks.
    confirming: Option<String>,
}

fn state_id() -> Id {
    Id::new("stamp-library")
}

fn load(ctx: &egui::Context) -> State {
    ctx.data(|d| d.get_temp::<State>(state_id()))
        .unwrap_or_default()
}

fn store(ctx: &egui::Context, state: State) {
    ctx.data_mut(|d| d.insert_temp(state_id(), state));
}

/// Open the browser on one half of the library.
///
/// A request left in egui's store rather than a field on `Editor`, for
/// `brushlib::take_draw_request`'s reason: the controls that open this are deep
/// inside the brush editor's modal, several layers of layout below the caller
/// that draws windows.
pub fn open(ctx: &egui::Context, kind: Kind) {
    let mut state = load(ctx);
    state.open = Some(kind);
    state.notice = None;
    state.confirming = None;
    state.query.clear();
    store(ctx, state);
}

/// Draw the browser.
///
/// Called from [`crate::panels::sidebars`] beside `brushlib::dialogs`, and for
/// exactly its reason: the layout can hide the Brushes panel, and a modal drawn
/// from inside a panel body cannot be shut and cannot be reopened.
pub fn dialogs(root: &mut Ui, p: &Palette, ed: &mut Editor) {
    let mut state = load(root.ctx());
    if state.open.is_some() {
        browser(root, p, ed, &mut state);
    }
    store(root.ctx(), state);
}

fn browser(root: &mut Ui, p: &Palette, ed: &mut Editor, state: &mut State) {
    let Some(kind) = state.open else {
        return;
    };
    // Clamped to the window, because a modal wider than the screen has no way
    // back out of its own corners.
    let available = root.ctx().content_rect().size();
    let [full_width, list_height] = metrics::STAMP_LIBRARY;
    let width = full_width.min(available.x - 48.0).max(360.0);
    let height = list_height.min(available.y - 200.0).max(160.0);

    let response = egui::Modal::new(Id::new("stamp-library-browser"))
        .frame(
            Frame::NONE
                .fill(p.window)
                .stroke(Stroke::new(1.0, p.popover_border))
                .corner_radius(10)
                .inner_margin(Margin::symmetric(22, 18)),
        )
        .show(root.ctx(), |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 8.0;
            header(ui, p, state);
            let kind = state.open.unwrap_or(kind);
            list(ui, p, ed, state, kind, height);
            footer(ui, p, ed, state, kind);
        });

    if response.should_close() {
        close(root.ctx(), state);
    }
}

/// Shut the browser and give its preview textures back.
///
/// The cache is emptied here rather than being trimmed as rows scroll: it holds
/// one 96-texel picture per row that has actually been drawn, which is
/// kilobytes while the modal is open and nothing at all once it is not. egui
/// frees the textures when the handles drop, and `app::submit_frame` does that
/// *after* `Queue::submit` — which is the rule that makes a same-frame free
/// legal at all.
fn close(ctx: &egui::Context, state: &mut State) {
    state.open = None;
    state.confirming = None;
    ctx.data_mut(|d| {
        d.remove::<Previews>(preview_id());
        d.remove::<Joins>(joins_id());
    });
}

fn header(ui: &mut Ui, p: &Palette, state: &mut State) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(
                RichText::new("Stamps & papers")
                    .size(15.0)
                    .color(p.text_strong)
                    .strong(),
            );
            controls::note(
                ui,
                p,
                "The pictures a brush paints through. Choosing one here puts it \
                 on the brush in your hand; press Update in the brush editor to \
                 keep it.",
            );
        });
        ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
            // Through `close`, not by clearing the flag: that is what gives the
            // preview textures and the seam answers back, and both hold
            // **full-resolution** `Arc<TipMask>`s — up to four megabytes each.
            // Setting `open = None` here left every picture the user had looked
            // at pinned in egui's store for the rest of the session, including
            // ones since removed from the library. One exit, one teardown.
            if icon_button(ui, p, Icon::Close, true, "Close") {
                close(ui.ctx(), state);
            }
        });
    });

    ui.horizontal(|ui| {
        let mut kind = state.open.unwrap_or(Kind::Stamps);
        // `widgets::segmented` and not a pair of dropdowns: two lists is a
        // choice between two, which is what a segmented control is for, and it
        // is the same control the brush editor's own tabs use.
        if widgets::segmented(
            ui,
            p,
            &mut kind,
            &[(Kind::Stamps, "Stamps"), (Kind::Papers, "Papers")],
        ) {
            state.open = Some(kind);
            // The query belongs to the list it was typed into: carrying it
            // across would show an empty half and no reason for it.
            state.query.clear();
            state.confirming = None;
        }
    });
    controls::search_field(ui, p, &mut state.query, "Search by name");
}

/// One row's worth of what the list needs, gathered before anything is drawn.
struct Entry {
    name: String,
    mask: Arc<TipMask>,
    source: Source,
    /// The brushes in the user's library still naming it — empty for a shipped
    /// picture, which no user brush can take away.
    users: Vec<String>,
}

/// Everything in one half of the library, the user's first.
///
/// Built once per frame the modal is open, which is the frame budget this can
/// afford: the merged list is at most a few dozen entries, against the 239
/// presets the brush browser refuses to rebuild per frame. It is not on the
/// drawing path — the modal is not open while anybody is painting.
fn entries(ed: &Editor, state: &State, kind: Kind) -> Vec<Entry> {
    let (yours, shipped): (&std::collections::BTreeMap<_, _>, _) = match kind {
        Kind::Stamps => (&ed.tips, umber_core::tip::builtin_tips()),
        Kind::Papers => (&ed.papers, umber_core::tip::patterns()),
    };
    let query = state.query.trim().to_ascii_lowercase();
    let matches = |name: &str| query.is_empty() || name.to_ascii_lowercase().contains(&query);

    let mut out: Vec<Entry> = Vec::with_capacity(yours.len() + shipped.len());
    for (name, mask) in yours {
        if !matches(name) {
            continue;
        }
        out.push(Entry {
            name: name.clone(),
            mask: Arc::clone(mask),
            source: Source::Yours,
            users: Vec::new(),
        });
    }
    for (name, mask) in shipped {
        if !matches(name) {
            continue;
        }
        out.push(Entry {
            name: (*name).to_owned(),
            mask: Arc::clone(mask),
            source: if yours.contains_key(*name) {
                Source::Hidden
            } else {
                Source::Shipped
            },
            users: Vec::new(),
        });
    }
    out
}

fn list(ui: &mut Ui, p: &Palette, ed: &mut Editor, state: &mut State, kind: Kind, height: f32) {
    if let Some(notice) = state.notice.clone()
        && brushlib::notice_bar(ui, p, &notice, true)
    {
        state.notice = None;
    }

    // The library **before** the list is built from it, and the order matters
    // exactly once: on the first frame of a fresh context this is what reads
    // the collection off disk, and `resync` is what fills `Editor::tips` and
    // `Editor::papers` from it. Built the other way round, the browser's first
    // frame showed the shipped pictures alone and the user's appeared on the
    // second — a flash nobody could account for.
    let held = brushlib::library(ui.ctx(), ed).ok();
    let mut rows = entries(ed, state, kind);
    // Who is using each of the user's own, which is what decides whether Remove
    // may be offered. Asked once per frame rather than once per row, and only
    // of the rows that can be removed at all.
    let in_hand = match kind {
        Kind::Stamps => ed.tip_name.clone(),
        Kind::Papers => ed.paper_name.clone(),
    };
    if let Some(library) = &held {
        for row in rows.iter_mut().filter(|r| r.source == Source::Yours) {
            row.users = match kind {
                Kind::Stamps => library.tip_users(&row.name),
                Kind::Papers => library.paper_users(&row.name),
            };
            // The brush the artist is holding counts, and the model cannot see
            // it — `UserLibrary` knows only what has been saved. Without this,
            // the picture under the pointer could be deleted with a cheerful
            // notice and the next stroke would silently change.
            if in_hand.as_deref() == Some(row.name.as_str()) {
                row.users.push("the brush in your hand".to_owned());
            }
        }
    }

    let mut action = None;
    let area = egui::ScrollArea::vertical()
        .id_salt("stamp-library-list")
        .auto_shrink([false, false])
        .max_height(height);

    if rows.is_empty() {
        area.show(ui, |ui| {
            // The first of these is reachable only where every shipped picture
            // failed to decode, which is exactly when a blank box would be
            // least informative — the library says so in a warning at load, and
            // this is what the browser shows for it.
            controls::note(
                ui,
                p,
                if state.query.trim().is_empty() {
                    "Nothing here yet. Import a picture to start."
                } else {
                    "Nothing of that name."
                },
            );
        });
    } else {
        // **`show_rows`, not `show`**, and that is not a nicety: laying a row
        // out builds its thumbnail by box-averaging the *whole* picture — up to
        // four million texels — uploads a texture, and for a paper walks the
        // tile again for its seam. Twenty large pictures at once is tens of
        // millions of texel reads and twenty uploads in a single frame, which
        // is the cost `brushlib`'s own list refuses at 201 presets. The caches
        // make every *later* frame free; this is what makes the first one
        // bearable. It is legal here because every row is the same height by
        // construction — a fixed square and two lines beside it, neither of
        // which wraps.
        area.show_rows(ui, metrics::STAMP_ROW, rows.len(), |ui, visible| {
            ui.spacing_mut().item_spacing.y = ROW_GAP;
            for entry in &rows[visible] {
                if let Some(chosen) = row(ui, p, ed, state, kind, entry) {
                    action = Some(chosen);
                }
            }
        });
    }

    match action {
        Some(Action::Use(name)) => choose(ed, state, kind, name),
        Some(Action::Remove(name)) => remove(ui.ctx(), ed, state, kind, name),
        None => {}
    }
}

enum Action {
    Use(String),
    Remove(String),
}

fn row(
    ui: &mut Ui,
    p: &Palette,
    ed: &Editor,
    state: &State,
    kind: Kind,
    entry: &Entry,
) -> Option<Action> {
    let mut action = None;
    // Which row the brush is actually painting with. A paper is compared by the
    // **tile**, not by the name: one of the shipped three can be in force
    // either as `Brush::grain_pattern` or as a name, and a name comparison
    // would leave the row unmarked in one of those spellings while the brush
    // painted through it. `paper_tile` is the same answer the dab pass binds.
    let chosen = match kind {
        Kind::Stamps => ed.tip_name.as_deref() == Some(entry.name.as_str()),
        Kind::Papers => ed
            .paper_tile()
            .is_some_and(|tile| Arc::ptr_eq(&tile, &entry.mask)),
    } && entry.source != Source::Hidden;

    Frame::NONE
        .fill(if chosen { p.control } else { p.window })
        .stroke(Stroke::new(
            1.0,
            if chosen { p.accent_dim } else { p.border },
        ))
        .corner_radius(metrics::RADIUS)
        .inner_margin(Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                preview(ui, p, kind, &entry.name, &entry.mask);
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(
                        RichText::new(&entry.name)
                            .size(text::SMALL)
                            .color(p.text_strong),
                    );
                    let joins = joins(ui, kind, &entry.name, &entry.mask);
                    ui.label(
                        RichText::new(detail(entry, joins))
                            .size(text::TINY)
                            .color(p.text_dim),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    // Remove is the user's own pictures only: a shipped one is
                    // in the binary and there is nothing on disk to take away.
                    if entry.source == Source::Yours {
                        let free = entry.users.is_empty();
                        let asking = state.confirming.as_deref() == Some(entry.name.as_str());
                        let label = if asking { "Really?" } else { "Remove" };
                        let tip = if free {
                            "Delete this picture from your library"
                        } else {
                            // Named rather than counted: "used by 2 brushes" is
                            // a number somebody then has to go and find.
                            &format!("Still used by {}", entry.users.join(", "))
                        };
                        if controls::text_button(ui, p, label, false, free)
                            .on_hover_text(tip)
                            .on_disabled_hover_text(tip)
                            .clicked()
                        {
                            action = Some(Action::Remove(entry.name.clone()));
                        }
                    }
                    if entry.source != Source::Hidden
                        && controls::text_button(ui, p, "Use", chosen, true)
                            .on_hover_text(kind.use_tip())
                            .clicked()
                    {
                        action = Some(Action::Use(entry.name.clone()));
                    }
                });
            });
        });
    action
}

/// The line under a picture's name: how big it is, where it came from, and —
/// for a paper — whether it joins to itself.
///
/// `joins` is `None` for a stamp, which is never asked the question: it is
/// stretched over one dab and has no second copy of itself to meet.
fn detail(entry: &Entry, joins: Option<bool>) -> String {
    let size = format!("{} × {} px", entry.mask.width(), entry.mask.height());
    let source = match entry.source {
        Source::Yours => "yours",
        Source::Shipped => "shipped with Umber",
        // The one row that has to explain itself: it is drawn so that a shipped
        // picture is never simply missing, and it cannot be chosen because a
        // name resolves to the user's copy first.
        Source::Hidden => "shipped, hidden by one of yours with the same name",
    };
    match joins {
        Some(false) => format!("{size} · {source} · does not tile, so it draws a grid"),
        _ => format!("{size} · {source}"),
    }
}

/// Whether a tile joins to itself, worked out once per tile rather than once
/// per frame.
///
/// [`umber_core::tip::seams`] walks the whole picture, which is 65 000 texel
/// pairs for a 256-square and four million for the largest a library may hold.
/// The modal redraws every frame and the answer cannot change while it is open
/// — the tiles are `Arc`s of immutable masks — so leaving the call in the row
/// would put a full pass per visible paper into every frame, for a sentence
/// that does not change. Cached beside the previews, keyed the same way and
/// validated by the same `Arc` identity, and thrown away with them when the
/// browser shuts.
type Joins = std::collections::HashMap<String, (Arc<TipMask>, bool)>;

fn joins_id() -> Id {
    Id::new("stamp-library-joins")
}

fn joins(ui: &Ui, kind: Kind, name: &str, mask: &Arc<TipMask>) -> Option<bool> {
    if kind != Kind::Papers {
        return None;
    }
    let key = format!("{}:{name}", kind.title());
    let cached = ui
        .ctx()
        .data(|d| d.get_temp::<Joins>(joins_id()))
        .and_then(|held| held.get(&key).cloned());
    if let Some((held, answer)) = cached
        && Arc::ptr_eq(&held, mask)
    {
        return Some(answer);
    }
    let answer = umber_core::tip::seams(mask).tiles();
    ui.ctx().data_mut(|d| {
        d.get_temp_mut_or_default::<Joins>(joins_id())
            .insert(key, (Arc::clone(mask), answer));
    });
    Some(answer)
}

// ---------------------------------------------------------------------------
// Previews
// ---------------------------------------------------------------------------

/// Widest a picture is downsampled to for a row's square.
///
/// A stamp can be 2048 texels across, so it is box-averaged down first —
/// nearest sampling shows a sparse spatter as an empty square about half the
/// time.
const PREVIEW_TEXELS: u32 = 96;

/// The row previews, by kind and name, validated by `Arc` identity.
type Previews = std::collections::HashMap<String, (Arc<TipMask>, egui::TextureHandle)>;

fn preview_id() -> Id {
    Id::new("stamp-library-previews")
}

/// One picture, in a square.
///
/// **Its own cache, deliberately not `brushlib::tip_preview`'s.** That one
/// holds a single slot, and it is drawn from the brush editor's Tip row — which
/// can be on screen at the same time as this modal, since this is opened from
/// it. Two consumers of a one-slot cache evict each other's live texture every
/// frame, which is a `wgpu` validation failure and not merely waste; the key
/// here therefore carries the *kind and name* as well as the mask, and this
/// module's own id keeps it out of that one's way entirely.
///
/// A paper is drawn **tiled two by two**, which is the whole reason the square
/// is worth looking at: a seam is invisible in one copy of a tile and obvious
/// the moment it meets itself.
fn preview(ui: &mut Ui, p: &Palette, kind: Kind, name: &str, mask: &Arc<TipMask>) {
    let side = metrics::STAMP_PREVIEW;
    let (rect, _) = ui.allocate_exact_size(vec2(side, side), Sense::hover());
    ui.painter().rect_filled(rect, metrics::RADIUS, p.chrome);

    let key = format!("{}:{name}", kind.title());
    let cached: Option<(Arc<TipMask>, egui::TextureHandle)> = ui
        .ctx()
        .data(|d| d.get_temp::<Previews>(preview_id()))
        .and_then(|held| held.get(&key).cloned());
    let texture = match cached {
        Some((held, texture)) if Arc::ptr_eq(&held, mask) => texture,
        _ => {
            let texture = ui.ctx().load_texture(
                format!("stamp-{key}"),
                widgets::tip_image(mask, p.text_strong, PREVIEW_TEXELS),
                egui::TextureOptions::LINEAR,
            );
            ui.ctx().data_mut(|d| {
                d.get_temp_mut_or_default::<Previews>(preview_id())
                    .insert(key, (Arc::clone(mask), texture.clone()));
            });
            texture
        }
    };

    // Two copies across and two down for a paper, one for a stamp: the sampler
    // that draws a paper on the canvas repeats and the one that draws a stamp
    // does not, so this is the preview showing what each really does.
    //
    // Drawn as N separate images rather than as one with a uv range past 1.
    // egui's textures are **clamped**, not repeating, so the wide uv magnifies
    // the top-left quarter and smears its edge row across the rest — which is
    // the one thing this square must not do, since it is here to show a join.
    tiled(ui, texture.id(), rect.shrink(2.0), kind.repeats());
}

/// Draw one texture `repeats` times across and down inside `rect`.
///
/// Shared with `ui::paper_preview`, which shows the same tiles at a different
/// size in the brush editor. One function rather than two, because the two
/// squares have to *agree* about what a paper looks like — a seam visible in
/// one and not the other is worse than neither showing it.
pub(crate) fn tiled(ui: &Ui, texture: egui::TextureId, rect: Rect, repeats: u32) {
    let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    let step = vec2(
        rect.width() / repeats as f32,
        rect.height() / repeats as f32,
    );
    for row in 0..repeats {
        for column in 0..repeats {
            let corner = rect.min + vec2(column as f32 * step.x, row as f32 * step.y);
            ui.painter().image(
                texture,
                Rect::from_min_size(corner, step),
                full,
                egui::Color32::WHITE,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// What a click does
// ---------------------------------------------------------------------------

/// Put the chosen picture on the brush in hand.
///
/// **Nothing is written.** Every other control in the brush editor changes the
/// brush and waits for Update, and a stamp that persisted on the spot would be
/// the one setting that behaved differently — worse, the one that could not be
/// undone by walking away. The picture is already in the library; what is being
/// chosen here is only which of them this brush names.
fn choose(ed: &mut Editor, state: &mut State, kind: Kind, name: String) {
    match kind {
        Kind::Stamps => {
            // The resolution order `Editor::apply_preset` uses, for its reason:
            // the user's library first, then what Umber ships.
            let mask = ed
                .tips
                .get(&name)
                .cloned()
                .or_else(|| umber_core::tip::builtin(&name).cloned());
            match mask {
                Some(mask) => {
                    ed.set_tip(mask, Some(name.clone()));
                    state.notice = Some(Notice::good(format!(
                        "The brush in your hand now stamps \"{name}\". \
                         Press Update in the brush editor to keep it."
                    )));
                }
                None => {
                    state.notice = Some(Notice::bad(format!(
                        "\"{name}\" is no longer in your library, so the brush paints round."
                    )));
                }
            }
        }
        Kind::Papers => {
            // One of the shipped three goes on the *enum*, not on the name, and
            // that is not a shortcut: the Texture section's dropdown spells it
            // that way, and two doors leaving the brush in two different states
            // that paint identically is two states to keep in step for ever —
            // the saved preset would differ depending on which control was used.
            // Only where the user's own library does not hold that name, since
            // theirs wins in every resolution and would make the enum a lie.
            match shipped_pattern(&name).filter(|_| !ed.papers.contains_key(&name)) {
                Some(pattern) => {
                    ed.brush.grain_pattern = pattern;
                    ed.set_paper(None);
                }
                None => ed.set_paper(Some(name.clone())),
            }
            // A paper the brush cannot feel is worth saying out loud: the
            // control did what it says and the mark will not change, which
            // looks exactly like a control that does nothing.
            let biting = ed.brush.has_grain();
            state.notice = Some(Notice::good(if biting {
                format!(
                    "The brush in your hand now paints through \"{name}\". \
                     Press Update in the brush editor to keep it."
                )
            } else {
                format!(
                    "The brush in your hand now names \"{name}\", but its Paper \
                     setting is at zero. Raise it in the brush editor's Texture \
                     section to let the grain bite."
                )
            }));
        }
    }
}

/// The `GrainPattern` a shipped tile's name belongs to, if it is one of theirs.
///
/// Read off `GrainPattern::key` rather than written out again, so a fourth
/// shipped paper cannot end up named in one place and not the other.
fn shipped_pattern(name: &str) -> Option<umber_core::GrainPattern> {
    umber_core::GrainPattern::ALL
        .into_iter()
        .find(|pattern| pattern.key() == name)
}

/// Take a picture out of the library, asking once first.
fn remove(ctx: &egui::Context, ed: &mut Editor, state: &mut State, kind: Kind, name: String) {
    if state.confirming.as_deref() != Some(name.as_str()) {
        state.confirming = Some(name);
        return;
    }
    state.confirming = None;
    let taken = brushlib::edit_library(ctx, ed, |library| match kind {
        Kind::Stamps => library.remove_tip(&name),
        Kind::Papers => library.remove_paper(&name),
    });
    state.notice = Some(match taken {
        Ok(Removed::Gone) => Notice::good(format!("Removed \"{name}\" from your library.")),
        // The model refuses as well as the button being disabled, because it
        // can see the whole library and a row can only see what it drew.
        Ok(Removed::InUse(users)) => Notice::bad(format!(
            "\"{name}\" is still used by {}, so it was left where it is.",
            users.join(", ")
        )),
        Ok(Removed::Unknown) => Notice::bad(format!("\"{name}\" is not in your library.")),
        Err(why) => Notice::bad(why),
    });
}

// ---------------------------------------------------------------------------
// Importing
// ---------------------------------------------------------------------------

fn footer(ui: &mut Ui, p: &Palette, ed: &mut Editor, state: &mut State, kind: Kind) {
    let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(line, 0.0, p.border);
    ui.add_space(2.0);

    ui.horizontal(|ui| {
        if controls::text_button(ui, p, "Import a picture…", true, true)
            .on_hover_text(match kind {
                Kind::Stamps => "Read a PNG, JPEG, TIFF, GIF or BMP as a stamp",
                Kind::Papers => "Read a PNG, JPEG, TIFF, GIF or BMP as a paper",
            })
            .clicked()
        {
            import(ui.ctx(), ed, state, kind);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let path = brushlib::library(ui.ctx(), ed)
                .map(|library| library.dir().display().to_string())
                .unwrap_or_else(|why| why);
            ui.add(egui::Label::new(RichText::new(&path).size(10.0).color(p.text_dim)).truncate())
                .on_hover_text(path.as_str());
        });
    });
}

/// Read a picture off disk into the library.
///
/// **This one does write, and it is the only control here that does** — the
/// argument is [`UserLibrary::add_tip`]'s: a picture imported into a brush's
/// hand can be discarded by selecting another brush, and a picture imported
/// into the library cannot be discarded by anything except taking it out again.
/// So it goes in, is recorded as one the user put there, and survives the next
/// save naming it nowhere.
///
/// Blocking, like every other file dialog in the interface.
fn import(ctx: &egui::Context, ed: &mut Editor, state: &mut State, kind: Kind) {
    let Some(path) = rfd::FileDialog::new()
        .set_title(match kind {
            Kind::Stamps => "Choose a picture to use as a stamp",
            Kind::Papers => "Choose a picture to use as a paper",
        })
        .add_filter(
            "Pictures",
            &["png", "jpg", "jpeg", "tif", "tiff", "gif", "bmp"],
        )
        .add_filter("All files", &["*"])
        .pick_file()
    else {
        return;
    };
    let stem = path
        .file_stem()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned();

    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) => {
            state.notice = Some(Notice::bad(format!("{}: {e}", path.display())));
            return;
        }
    };

    match kind {
        Kind::Stamps => {
            let read = TipMask::from_picture(&bytes);
            let (mask, reading) = match read {
                Ok(pair) => pair,
                Err(e) => {
                    state.notice = Some(Notice::bad(e.to_string()));
                    return;
                }
            };
            let (w, h) = (mask.width(), mask.height());
            match brushlib::edit_library(ctx, ed, |library| library.add_tip(&stem, mask)) {
                // Which reading was taken is said every time, because it is a
                // guess — see `umber_core::TipReading`.
                Ok(name) => {
                    state.notice = Some(Notice::good(format!(
                        "Added \"{name}\" ({w} × {h}) to your stamps, by reading {}.",
                        reading.describe()
                    )));
                }
                Err(why) => state.notice = Some(Notice::bad(why)),
            }
        }
        Kind::Papers => {
            let tile = match TipMask::from_paper(&bytes) {
                Ok(tile) => tile,
                Err(e) => {
                    state.notice = Some(Notice::bad(e.to_string()));
                    return;
                }
            };
            let (w, h) = (tile.width(), tile.height());
            // Measured before the tile is handed over, and reported rather than
            // refused. Textures authored for a painting application are made to
            // tile, so a refusal would turn away most of what people have; and
            // a grid drawn across somebody's canvas by a texture that does not
            // is exactly the subtly-wrong-pixels failure a notice exists for.
            // Mirroring it into place was the third option and is worse than
            // either: it silently doubles the tile and swaps a seam for an axis
            // of symmetry running through every stroke, which is a different
            // artefact rather than none.
            let joins = umber_core::tip::seams(&tile).tiles();
            match brushlib::edit_library(ctx, ed, |library| library.add_paper(&stem, tile)) {
                Ok(name) => {
                    state.notice = Some(if joins {
                        Notice::good(format!(
                            "Added \"{name}\" ({w} × {h}) to your papers. \
                             Brightness is the grain: white keeps the whole mark, \
                             black takes it away."
                        ))
                    } else {
                        Notice::bad(format!(
                            "Added \"{name}\" ({w} × {h}) to your papers, but its edges \
                             do not meet. The grain is anchored to the document and \
                             repeats across it, so this one will draw a grid over the \
                             canvas, one line every tile."
                        ))
                    });
                }
                Err(why) => state.notice = Some(Notice::bad(why)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tile with a hard join down the middle, which is what an unprepared
    /// photograph of paper looks like.
    fn split_tile(side: u32) -> Arc<TipMask> {
        let mut texels = vec![0u8; (side * side) as usize];
        for y in 0..side {
            for x in 0..side {
                texels[(y * side + x) as usize] = if x < side / 2 { 40 } else { 220 };
            }
        }
        Arc::new(TipMask::new(side, side, texels).expect("tile"))
    }

    /// The browser, both halves, and the brush editor's Texture section with a
    /// paper of the user's own on it.
    ///
    /// Written rather than asserted for the reason `palette_module_preview` is:
    /// what can go wrong in a list of rows carrying a picture, two labels and
    /// two buttons inside a 520 px modal is a *layout*, and no assertion about
    /// widgets catches controls drawn over each other. The tiled preview is the
    /// other half of why this exists — a seam is a thing to be looked at, and
    /// the shots are what say the square shows one.
    ///
    /// ```sh
    /// cargo test -p umber-app stamp_library_preview -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "writes preview PNGs and wants a GPU; run deliberately"]
    #[cfg(debug_assertions)]
    fn stamp_library_preview() {
        use crate::docshot;
        use crate::theme::Palette as Theme;
        use egui::vec2;

        let Some(mut stage) = docshot::Stage::new() else {
            eprintln!("no GPU adapter: nothing to draw into. Skipped.");
            return;
        };
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/stamp-library");
        std::fs::create_dir_all(&dir).expect("create the preview directory");

        // A scratch library, so the shot is a picture of the interface rather
        // than of whoever ran it — `brushlib::load` reads the *real* one on the
        // first frame of a fresh context. One of the papers deliberately does
        // not join, which is the row that has to say so and the case the tiled
        // preview exists to show.
        let scratch = std::env::temp_dir().join("umber-stamp-shot");
        let _ = std::fs::remove_dir_all(&scratch);
        let mut staged =
            umber_core::preset::UserLibrary::load_from(&scratch).expect("a scratch library");
        staged
            .add_paper("linen", (*split_tile(64)).clone())
            .expect("add");
        staged
            .add_paper(
                "wove",
                (**umber_core::tip::pattern("canvas").expect("shipped")).clone(),
            )
            .expect("add");
        staged
            .add_tip(
                "my-nib",
                (**umber_core::tip::builtin("umber-stipple").expect("shipped")).clone(),
            )
            .expect("add");

        for (name, kind) in [
            ("1-stamps", Some(Kind::Stamps)),
            ("2-papers", Some(Kind::Papers)),
            // The other end of the same feature: the Texture section, whose
            // paper control became a dropdown when the closed set of three
            // stopped being the whole list.
            ("3-texture-section", None),
            // And the Brushes header, which now carries four marks in a 264 px
            // panel. `CLAUDE.md` records the layers panel's version of exactly
            // this: controls that fit in the abstract and were drawn over each
            // other at the panel's real width.
            ("4-brushes-header", None),
        ] {
            let panel_shot = name == "4-brushes-header";
            let mut ed = Editor::default();
            ed.layout = crate::dock::Layout::default();
            ed.paper_name = Some("wove".to_owned());
            ed.brush.grain = 0.45;
            ed.ui.brush_editor_open = kind.is_none() && !panel_shot;
            ed.ui.brush_tab = crate::editor::BrushTab::Texture;

            let seed = State {
                open: kind,
                ..State::default()
            };
            let palette = Theme::with_accent(ed.ui.theme, ed.ui.accent);
            let field = match (kind, panel_shot) {
                (Some(_), _) => vec2(metrics::STAMP_LIBRARY[0] + 80.0, 560.0),
                (None, true) => vec2(metrics::PANEL, 360.0),
                (None, false) => vec2(metrics::BRUSH_EDITOR[0] + 120.0, 560.0),
            };
            let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, field);
            let staged = staged.clone();
            let image = stage.shoot(field, 2.0, &palette, palette.dock, |root| {
                // Re-seeded every frame: the state is read back out of egui's
                // memory, and a frame that had not been seeded would draw
                // nothing at all.
                store(root.ctx(), seed.clone());
                brushlib::seed_library(root.ctx(), &mut ed, staged.clone());
                if panel_shot {
                    let mut actions = crate::ui::UiActions::default();
                    crate::panels::panel(
                        root,
                        &palette,
                        &mut ed,
                        &mut actions,
                        crate::dock::PanelKind::Brushes,
                        rect,
                    );
                    return;
                }
                dialogs(root, &palette, &mut ed);
                crate::ui::brush_editor(root, &palette, &mut ed);
            });
            docshot::write_png(&dir.join(format!("{name}.png")), &image).expect("write the png");
        }
        let _ = std::fs::remove_dir_all(&scratch);
        println!("wrote 4 shots to {}", dir.display());
    }

    /// The list is the merged one and the user's own comes first, which is the
    /// order `Editor::paper_tile` and `Editor::apply_preset` resolve a name in
    /// — so a row that could be chosen has to be the row that would be found.
    #[test]
    fn the_users_own_pictures_come_first_and_a_shadowed_shipped_one_says_so() {
        let mut ed = Editor::default();
        let tile = Arc::new(TipMask::new(2, 2, vec![9; 4]).expect("tile"));
        ed.papers.insert("linen".to_owned(), Arc::clone(&tile));
        // Taking the name of one Umber ships.
        ed.papers.insert("tooth".to_owned(), tile);

        let state = State::default();
        let rows = entries(&ed, &state, Kind::Papers);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(&names[..2], ["linen", "tooth"], "yours come first");
        assert!(rows[0].source == Source::Yours && rows[1].source == Source::Yours);

        // Every shipped tile is still listed, and the shadowed one says why it
        // cannot be picked rather than quietly not being there.
        let shipped: Vec<&Entry> = rows.iter().filter(|r| r.source != Source::Yours).collect();
        assert_eq!(shipped.len(), umber_core::tip::patterns().len());
        let hidden: Vec<&Entry> = rows.iter().filter(|r| r.source == Source::Hidden).collect();
        assert_eq!(hidden.len(), 1);
        assert_eq!(hidden[0].name, "tooth");
        assert!(detail(hidden[0], Some(true)).contains("hidden"));
    }

    /// The one thing a single square cannot show and the one thing that ruins a
    /// painting. Reported on the row as well as at the import, because a tile
    /// can arrive by an import of somebody's brush rather than by this door.
    #[test]
    fn a_paper_that_does_not_join_says_so_on_its_own_row() {
        let mut ed = Editor::default();
        ed.papers.insert("photo".to_owned(), split_tile(16));
        let rows = entries(&ed, &State::default(), Kind::Papers);
        let row = rows.iter().find(|r| r.name == "photo").expect("listed");
        // The verdict the row's cache carries, computed the way `joins` does.
        let seamed = umber_core::tip::seams(&row.mask).tiles();
        assert!(!seamed);
        assert!(detail(row, Some(seamed)).contains("does not tile"));

        // A shipped tile does join, and is not nagged about — a notice on every
        // row is one nobody reads.
        let shipped = rows
            .iter()
            .find(|r| r.source == Source::Shipped)
            .expect("listed");
        let joins = umber_core::tip::seams(&shipped.mask).tiles();
        assert!(joins, "a shipped paper should tile");
        assert!(!detail(shipped, Some(joins)).contains("does not tile"));

        // A stamp is never asked the question: it is stretched over one dab and
        // has no second copy of itself to meet, which is what the `None` means.
        assert!(!detail(row, None).contains("tile"));
    }

    /// The browser and the Texture section's dropdown are two doors onto one
    /// choice, so they have to leave the brush in the *same* state — otherwise
    /// which control was used decides what the saved preset says, and two
    /// spellings that paint identically are two things to keep in step for
    /// ever.
    #[test]
    fn choosing_a_shipped_paper_sets_the_enum_the_dropdown_sets() {
        let mut ed = Editor::default();
        let mut state = State::default();

        choose(&mut ed, &mut state, Kind::Papers, "grit".to_owned());
        assert_eq!(ed.brush.grain_pattern, umber_core::GrainPattern::Grit);
        assert!(
            ed.paper_name.is_none(),
            "a shipped paper goes on the enum, not on the name"
        );

        // One of the user's own has nowhere else to go.
        ed.papers.insert(
            "linen".to_owned(),
            Arc::new(TipMask::new(2, 2, vec![3; 4]).expect("tile")),
        );
        choose(&mut ed, &mut state, Kind::Papers, "linen".to_owned());
        assert_eq!(ed.paper_name.as_deref(), Some("linen"));

        // And a tile of theirs that has taken a shipped name stays a name:
        // theirs wins in every resolution, so setting the enum would be a lie
        // about which picture is in force.
        ed.papers.insert(
            "tooth".to_owned(),
            Arc::new(TipMask::new(2, 2, vec![4; 4]).expect("tile")),
        );
        choose(&mut ed, &mut state, Kind::Papers, "tooth".to_owned());
        assert_eq!(ed.paper_name.as_deref(), Some("tooth"));
        assert!(Arc::ptr_eq(
            &ed.paper_tile().expect("theirs"),
            ed.papers.get("tooth").expect("held")
        ));
    }

    /// The search reaches both halves of the merged list, and folds case and
    /// surrounding space the way every other search in the interface does.
    #[test]
    fn the_search_folds_case_and_covers_both_halves() {
        let mut ed = Editor::default();
        ed.tips.insert(
            "Rough Nib".to_owned(),
            Arc::new(TipMask::new(2, 2, vec![1; 4]).expect("mask")),
        );
        let matched = |query: &str| {
            let state = State {
                query: query.to_owned(),
                ..State::default()
            };
            entries(&ed, &state, Kind::Stamps)
        };

        let rows = matched("  rough ");
        assert_eq!(rows[0].name, "Rough Nib", "yours first, whatever matched");
        assert!(rows.len() < matched("").len(), "nothing was filtered out");
        assert!(matched("nothing is called this").is_empty());

        // And the shipped half is searched by the same query box. The word is
        // taken *from* the shipped half rather than written down here: the
        // shipped table is generated, and a stamp named in a test is one that
        // silently stops being tested the day the library is retrimmed. It
        // already happened — this assertion used to search for "rough", which
        // matched a shipped stamp until eleven brushes and five masks left.
        let all = matched("");
        let shipped = all
            .iter()
            .find(|r| r.source == Source::Shipped)
            .expect("the shipped half of the merged list is not empty");
        let word = shipped
            .name
            .split(|c: char| !c.is_alphanumeric())
            .find(|w| w.len() > 3)
            .expect("a shipped stamp has a word in its name")
            .to_owned();
        assert!(
            matched(&word.to_uppercase())
                .iter()
                .any(|r| r.source == Source::Shipped),
            "the shipped half was not searched, for `{word}`"
        );
    }
}
