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

    /// What a row's Use button does, said before it is pressed.
    fn use_tip(self) -> &'static str {
        match self {
            Self::Stamps => "Stamp the brush in hand with this",
            Self::Papers => "Paint the brush in hand through this paper",
        }
    }
}

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
    ctx.data_mut(|d| d.remove::<Previews>(preview_id()));
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
            if icon_button(ui, p, Icon::Close, true, "Close") {
                state.open = None;
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

    let mut rows = entries(ed, state, kind);
    // Who is using each of the user's own, which is what decides whether Remove
    // may be offered. Asked once per frame rather than once per row, and only
    // of the rows that can be removed at all.
    if let Ok(library) = brushlib::library(ui.ctx(), ed) {
        for row in rows.iter_mut().filter(|r| r.source == Source::Yours) {
            row.users = match kind {
                Kind::Stamps => library.tip_users(&row.name),
                Kind::Papers => library.paper_users(&row.name),
            };
        }
    }

    let mut action = None;
    egui::ScrollArea::vertical()
        .id_salt("stamp-library-list")
        .auto_shrink([false, false])
        .max_height(height)
        .show(ui, |ui| {
            if rows.is_empty() {
                controls::note(
                    ui,
                    p,
                    if state.query.trim().is_empty() {
                        "Nothing here yet. Import a picture to start."
                    } else {
                        "Nothing of that name."
                    },
                );
                return;
            }
            ui.spacing_mut().item_spacing.y = 6.0;
            for entry in &rows {
                if let Some(chosen) = row(ui, p, ed, state, kind, entry) {
                    action = Some(chosen);
                }
            }
        });

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
    let chosen = match kind {
        Kind::Stamps => ed.tip_name.as_deref() == Some(entry.name.as_str()),
        Kind::Papers => ed.paper_name.as_deref() == Some(entry.name.as_str()),
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
                    ui.label(
                        RichText::new(detail(kind, entry))
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
fn detail(kind: Kind, entry: &Entry) -> String {
    let size = format!("{} × {} px", entry.mask.width(), entry.mask.height());
    let source = match entry.source {
        Source::Yours => "yours",
        Source::Shipped => "shipped with Umber",
        // The one row that has to explain itself: it is drawn so that a shipped
        // picture is never simply missing, and it cannot be chosen because a
        // name resolves to the user's copy first.
        Source::Hidden => "shipped — hidden by one of yours with the same name",
    };
    match kind {
        Kind::Papers if !umber_core::tip::seams(&entry.mask).tiles() => {
            format!("{size} · {source} · does not tile — shows a grid")
        }
        _ => format!("{size} · {source}"),
    }
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
    let repeats = match kind {
        Kind::Papers => 2.0,
        Kind::Stamps => 1.0,
    };
    ui.painter().image(
        texture.id(),
        rect.shrink(2.0),
        Rect::from_min_max(pos2(0.0, 0.0), pos2(repeats, repeats)),
        egui::Color32::WHITE,
    );
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
            ed.set_paper(Some(name.clone()));
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
                     setting is at zero — raise it in the brush editor's Texture \
                     section to let the grain bite."
                )
            }));
        }
    }
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
                             canvas — one line every tile."
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
        assert!(detail(Kind::Papers, hidden[0]).contains("hidden"));
    }

    /// The one thing a single square cannot show and the one thing that ruins a
    /// painting. Reported on the row as well as at the import, because a tile
    /// can arrive by an import of somebody's brush rather than by this door.
    #[test]
    fn a_paper_that_does_not_join_says_so_on_its_own_row() {
        let mut ed = Editor::default();
        let mut split = vec![0u8; 16 * 16];
        for y in 0..16 {
            for x in 0..16 {
                split[y * 16 + x] = if x < 8 { 30 } else { 230 };
            }
        }
        ed.papers.insert(
            "photo".to_owned(),
            Arc::new(TipMask::new(16, 16, split).expect("tile")),
        );
        let rows = entries(&ed, &State::default(), Kind::Papers);
        let row = rows.iter().find(|r| r.name == "photo").expect("listed");
        assert!(detail(Kind::Papers, row).contains("does not tile"));

        // A stamp is never asked the question: it is stretched over one dab and
        // has no second copy of itself to meet.
        ed.tips.insert("photo".to_owned(), Arc::clone(&row.mask));
        let stamps = entries(&ed, &State::default(), Kind::Stamps);
        let stamp = stamps.iter().find(|r| r.name == "photo").expect("listed");
        assert!(!detail(Kind::Stamps, stamp).contains("tile"));
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
        // A shipped stamp with the same word in it is found by the same query:
        // one list, one search.
        assert!(
            rows.iter().any(|r| r.source == Source::Shipped),
            "the shipped half was not searched"
        );
        assert!(rows.len() < matched("").len(), "nothing was filtered out");
        assert!(matched("nothing is called this").is_empty());
    }
}
