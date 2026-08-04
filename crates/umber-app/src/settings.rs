//! The settings dialog, from the design's Settings screen.
//!
//! The design's shape: a modal with a left rail of six tabs — General, Input &
//! pen, Pressure, Themes, Shortcuts, Performance — and one pane at a time on
//! the right. Themes and Shortcuts are the two the design draws in full; the
//! rest it marks as outside the prototype.
//!
//! Here, a tab is live only if there is something behind it that works. All but
//! Performance are; that one is shown disabled, with a tooltip saying why,
//! rather than opening onto a pane of controls that do nothing.
//!
//! **Pressure and Input & pen are one tab**, and the design's Pressure is the
//! one that goes. Everything there is to set about pressure is three buttons
//! and — under one of them — two sliders, and every one of them is only
//! meaningful against a reading of what the pointer is actually doing: what
//! "Device" is worth depends entirely on whether pen events are arriving at
//! all, and the whole reason to reach for "Speed" is that they are not. Split
//! across two tabs, the answer to "why is my pen painting flat?" lived on one
//! page and the knob for it on another, and a Change button had to carry the
//! reader between them. Together, the source is chosen a few lines above the
//! trace and the strip that show what it did.
//!
//! So Input & pen is the one pane that is not only settings. It reports, live,
//! what is arriving and what the pressure model makes of it — because that is
//! the one thing the machine this is written on cannot answer: nobody working
//! on Umber has a pen, so the two pen fixes it exists to verify shipped
//! unproven. See [`crate::inputlog`].
//!
//! **The dialog is one size on every pane.** [`WIDTH`] by [`HEIGHT`], clamped
//! only to the window, with one vertical scroll area between the pane's header
//! and its footer and never a horizontal one. It used to be sized by whichever
//! page was showing — two panes scrolled and two did not — so moving from
//! Themes to Shortcuts resized the modal and slid the rail out from under the
//! pointer that had just clicked it. Anything added here belongs *inside* that
//! scroll area, and nothing in it may report itself wider than the pane; see
//! the note on [`TextWrapMode`](egui::TextWrapMode) in [`pane`].
//!
//! The page also owns the preferences file. [`show`] runs every frame whether
//! the dialog is open or not, so its first call is where stored settings are
//! read, and the frame after a change is where they are written.

use crate::autosave;
use crate::controls::{self, CapState, Captured, Glyph};
use crate::editor::Editor;
use crate::icons::{self, Icon};
use crate::inputlog;
use crate::prefs;
use crate::shortcuts::{self, Action, Binding};
use crate::tabs::Notice;
use crate::theme::{Accent, Palette, ThemeKind, Token, TokenGroup, metrics, text};
use crate::themelib::{self, ThemeLibrary};
use crate::ui::UiActions;
use crate::widgets;
use egui::{Align2, Color32, FontId, Frame, Margin, Rect, Sense, Stroke, vec2};
use std::time::Duration;
use umber_core::input::PressureSource;

/// The panes of the settings dialog, in the design's rail order.
///
/// `Themes` keeps its name because the workspace's default tab names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsTab {
    General,
    /// Where pressure comes from, and a live reading of the pointer stream to
    /// judge it by. The design's Pressure tab folded into this one — see the
    /// module docs.
    InputAndPen,
    Themes,
    Shortcuts,
    /// Designed but not built.
    Performance,
}

impl SettingsTab {
    /// The rail, in the design's order, with the reason a tab is dead.
    const RAIL: [(SettingsTab, &'static str, &'static str); 5] = [
        (SettingsTab::General, "General", ""),
        (SettingsTab::InputAndPen, "Input & pen", ""),
        (SettingsTab::Themes, "Themes", ""),
        (SettingsTab::Shortcuts, "Shortcuts", ""),
        (
            SettingsTab::Performance,
            "Performance",
            "The frame counter in the menu bar is the whole of it so far. There \
             is no render budget, tile cache or prediction to tune yet.",
        ),
    ];
}

/// The design's dialog is 1000×640, and it is that size on every pane.
///
/// One size always, not a size per page. A modal that grew when you moved from
/// Themes to Shortcuts moved the rail out from under the pointer, so the tab
/// you had just clicked was no longer where you clicked it — and the pane that
/// happened to be longest silently decided how big the dialog was. What varies
/// is the *content*, and a page longer than the frame scrolls inside it.
///
/// Clamped to the window, because a modal wider than the screen has no way back
/// out of its own corners. That clamp reads the window and never the page, so
/// it cannot reintroduce the thing above.
///
/// These belong in `theme::metrics` with the design's other fixed sizes, beside
/// the brush browser's, which is there for exactly this reason. They are here
/// because this change does not own that file.
const WIDTH: f32 = 1000.0;
const HEIGHT: f32 = 640.0;
/// The design's left rail.
const RAIL_WIDTH: f32 = 190.0;
/// The dialog's corner radius. Shared with the rail, which reaches both left
/// corners and has to round with it.
const CORNER: u8 = 10;

/// Draw the dialog if it is open, and keep the preferences file in step.
pub fn show(root: &mut egui::Ui, p: &Palette, ed: &mut Editor, actions: &mut UiActions) {
    let ctx = root.ctx().clone();

    if prefs::ensure_loaded(&ctx, ed) {
        // The theme is pushed into egui's style *before* the UI runs, so a
        // theme read here does not take effect until the next frame — and under
        // `ControlFlow::Wait` that frame has to be asked for, or the app sits
        // in the default theme until the user happens to move the mouse.
        ctx.request_repaint();
    }
    prefs::flush_if_idle(&ctx, ed);

    // The test strip resolves pointer events through its own copy of the
    // pressure model for as long as it is being dragged, and the drag is ended
    // by a release the strip only hears about while it is on screen. Shutting
    // the dialog or changing tab mid-drag would otherwise leave it resolving
    // every event in the application for the rest of the session.
    if !ed.ui.settings_open || ed.ui.settings_tab != SettingsTab::InputAndPen {
        ed.input.end_probe();
    }
    // Nothing the Themes pane was in the middle of may outlive the page it was
    // started on. Same rule, and the same place, as ending the pressure probe
    // above; two things depend on it and both were bugs:
    //
    // - a "Delete?" still armed when somebody walks away to Shortcuts and back
    //   is a control that takes a theme on the next click, for a question they
    //   answered a page ago.
    // - **Escape does not abandon a field**, whatever a text editor's habits
    //   suggest: egui's `TextEdit` handles no `Key::Escape` at all, and
    //   `egui::Modal` consumes it to close the dialog. So a half-typed name, or
    //   a hex that says `rebeccapurple`, is left in the buffer with the field
    //   never drawn again — and reopening the page shows a readout the chip
    //   beside it disagrees with, whose *next* blur applies the rename the user
    //   thought they had cancelled.
    if !ed.ui.settings_open || ed.ui.settings_tab != SettingsTab::Themes {
        forget_themes_edit(&ctx);
    }

    if !ed.ui.settings_open {
        // Closing the dialog while a field was listening would otherwise leave
        // the canvas deaf to every shortcut.
        stop_listening(&ctx);
        return;
    }

    let available = ctx.content_rect().size();
    let width = WIDTH.min(available.x - 48.0).max(420.0);
    let height = HEIGHT.min(available.y - 48.0).max(320.0);

    let response = egui::Modal::new(egui::Id::new("settings"))
        .frame(
            Frame::NONE
                .fill(p.window)
                .stroke(Stroke::new(1.0, p.popover_border))
                .corner_radius(CORNER)
                .inner_margin(Margin::ZERO),
        )
        .show(root.ctx(), |ui| {
            ui.set_width(width);
            ui.set_height(height);

            ui.horizontal_top(|ui| {
                // No gutter between the rail and the pane: the design butts
                // them together with a hairline, which the rail's own fill
                // provides.
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.allocate_ui_with_layout(
                    vec2(RAIL_WIDTH, height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| rail(ui, p, ed),
                );
                ui.allocate_ui_with_layout(
                    vec2(width - RAIL_WIDTH, height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| pane(ui, p, ed, actions),
                );
            });
        });

    if response.should_close() {
        ed.ui.settings_open = false;
        stop_listening(&ctx);
    }
}

/// Gap between the footer's hairline and the row under it.
const FOOTER_GAP: f32 = 8.0;

/// Height the footer — the hairline, the path and the reset button — claims at
/// the bottom of every pane.
///
/// Named because two places have to agree on it: the pane, which pushes the
/// footer down by whatever is left, and the Shortcuts list, which grows to fill
/// that same space and would otherwise slide underneath it.
///
/// **Added up from its parts rather than estimated**, and that is the fix for a
/// real bug. It was 34 while the footer actually cost 47 — a hairline, two
/// inherited eight-point item gaps, the eight-point gap of its own and a
/// 22-point button — so the pane came out thirteen points taller than the
/// height it had been handed. The rail beside it is exactly that height, the
/// modal grows to the taller of the two, and the result was a left sidebar that
/// stopped short of the bottom of the dialog with the version and licence
/// floating above it. `storage_footer` takes its own vertical spacing to zero
/// so the three parts here are all it costs.
const FOOTER_RESERVE: f32 = 1.0 + FOOTER_GAP + metrics::TEXT_BUTTON;

/// Breathing space between a pane's last control and the footer's hairline.
const LIST_GAP: f32 = 12.0;

/// Gap between the theme cards.
const CARD_GAP: f32 = 12.0;

/// One theme card: the miniature workspace and the name strip under it.
const CARD: [f32; 2] = [150.0, 104.0];

/// Between the theme editor's two columns of tokens.
///
/// Named because the columns are sized from what is left once it has been taken
/// off, and the two figures have to be the same one — see `theme_editor`.
const TOKEN_GAP: f32 = 24.0;

/// The hex field on a token row. Wide enough for `#RRGGBB` in the monospace
/// face with room for a caret at the end of it.
const HEX_FIELD: f32 = 56.0;

/// The name field in the theme editor's heading.
const NAME_FIELD: f32 = 160.0;

/// A text field dressed as the readout it stands in for.
///
/// A bare `egui::TextEdit` is invisible here: its fill is `extreme_bg_color`,
/// which `theme::apply` sets to `Palette::window`, which is exactly what the
/// theme editor's box is filled with — so the field read as a label and the one
/// editable thing on the page looked like the read-only inspector it replaced.
/// A well of its own, in the same shapes `controls::search_field` uses.
fn inset_field(
    ui: &mut egui::Ui,
    p: &Palette,
    buffer: &mut String,
    width: f32,
    font: FontId,
) -> egui::Response {
    let mut response = None;
    Frame::NONE
        .fill(p.control)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(metrics::RADIUS)
        .inner_margin(Margin::symmetric(6, 2))
        .show(ui, |ui| {
            response = Some(
                ui.add(
                    egui::TextEdit::singleline(buffer)
                        .frame(egui::Frame::NONE)
                        .desired_width(width)
                        .font(font)
                        .text_color(p.text_strong),
                ),
            );
        });
    response.expect("a frame always runs its body")
}

/// The design's left rail: title, tabs, then the version at the foot.
fn rail(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    Frame::NONE
        .fill(p.chrome)
        // The rail is the full height of the dialog and runs into both left
        // corners, so it has to carry the dialog's own radius there. A square
        // fill under a rounded frame does not get clipped by it — it paints
        // over the corner, and the chrome pokes out past the border.
        .corner_radius(egui::CornerRadius {
            nw: CORNER,
            sw: CORNER,
            ne: 0,
            se: 0,
        })
        .inner_margin(Margin::symmetric(8, 16))
        .show(ui, |ui| {
            ui.set_min_height(ui.available_height());
            ui.spacing_mut().item_spacing.y = 2.0;

            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new("Settings")
                        .size(text::HEADING)
                        .color(p.text_strong)
                        .strong(),
                );
            });
            ui.add_space(12.0);

            for (tab, label, disabled_reason) in SettingsTab::RAIL {
                let live = disabled_reason.is_empty();
                if controls::sidebar_tab(ui, p, label, ed.ui.settings_tab == tab, live, {
                    disabled_reason
                })
                .clicked()
                {
                    ed.ui.settings_tab = tab;
                    stop_listening(ui.ctx());
                }
            }

            // Push the version note to the foot of the rail, as the design has
            // it. `allocate_space` rather than a spacer widget so it takes
            // exactly whatever is left.
            let left = (ui.available_height() - 20.0).max(0.0);
            ui.allocate_space(vec2(0.0, left));
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(
                    // An em dash, not a middle dot: the workspace title already
                    // proves Archivo carries this one.
                    egui::RichText::new(format!("v{} — GPL-3.0", env!("CARGO_PKG_VERSION")))
                        .size(10.0)
                        .color(p.text_dim.gamma_multiply(0.7)),
                );
            });
        });
}

/// A pane's own padding, inside the column the dialog hands it.
///
/// Named because the footer's reserve is measured against the width that is
/// left *after* it, and a test taking the bare column would be measuring in
/// fifty-six points the footer does not have.
const PANE_MARGIN: i8 = 28;

fn pane(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor, actions: &mut UiActions) {
    Frame::NONE
        .inner_margin(Margin::symmetric(PANE_MARGIN, 24))
        .show(ui, |ui| {
            ui.set_min_height(ui.available_height());
            ui.spacing_mut().item_spacing.y = 8.0;

            pane_header(ui, p, ed);
            ui.add_space(10.0);

            // The one scroll area in the dialog, and the whole of how the frame
            // stays one size. `auto_shrink([false, false])` makes it claim the
            // space it was given whatever is in it, so a short pane does not
            // shrink the dialog and a long one does not stretch it; and because
            // it is `vertical`, content wider than the viewport is clipped
            // rather than growing a horizontal bar. Every pane used to decide
            // its own answer to this — two of them scrolled and two did not,
            // which is exactly why the dialog changed size as you moved between
            // them.
            let body = (ui.available_height() - FOOTER_RESERVE - LIST_GAP).max(0.0);
            egui::ScrollArea::vertical()
                // A scroll position per pane. One shared position would carry
                // the Shortcuts list's offset onto General, which is short
                // enough to be left showing nothing.
                .id_salt(("settings-pane", pane_id(ed.ui.settings_tab)))
                .max_height(body)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // The trap this dialog kept falling into: an egui label in a
                    // *horizontal* layout defaults to `TextWrapMode::Extend`, so
                    // a long one does not run onto a second line — it makes its
                    // row wider, and with it the pane, and with it the window.
                    // `set_max_width` does not help, because an extending label
                    // overruns the ui it is in. Wrapping is the fix at the
                    // source; the clip above is what makes a page that still
                    // overruns cost a truncated word rather than the dialog's
                    // size.
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                    ui.spacing_mut().item_spacing.y = 8.0;

                    match ed.ui.settings_tab {
                        SettingsTab::General => general_pane(ui, p, ed, actions),
                        SettingsTab::InputAndPen => input_pane(ui, p, ed),
                        SettingsTab::Themes => themes_pane(ui, p, ed),
                        SettingsTab::Shortcuts => shortcuts_pane(ui, p),
                        // The rail cannot select this; a preferences file naming
                        // it could, so it lands somewhere rather than on a blank
                        // pane.
                        SettingsTab::Performance => ed.ui.settings_tab = SettingsTab::General,
                    }
                });

            // The footer goes in the bottom `FOOTER_RESERVE` points of whatever
            // is left, and the pane consumes *exactly* what it was handed. It
            // used to `allocate_space` a gap and then let the footer size
            // itself, which came out thirteen points over — and since the rail
            // is exactly the dialog's height, the modal grew to the taller of
            // the two and the rail stopped short of the bottom. The spacing is
            // taken to zero first because an item gap between the last two
            // widgets is the sort of thing that puts an exact figure out by
            // eight; `new_child` does not advance this `Ui`, so the footer
            // cannot grow it whatever ends up in it.
            ui.spacing_mut().item_spacing.y = 0.0;
            let rest = (ui.available_height()).max(0.0);
            let (region, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), rest), Sense::hover());
            let mut footer = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(Rect::from_min_size(
                        egui::pos2(region.left(), region.bottom() - FOOTER_RESERVE),
                        vec2(region.width(), FOOTER_RESERVE),
                    ))
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            storage_footer(&mut footer, p, ed);
        });
}

/// A stable name for a pane, so its scroll position is its own.
fn pane_id(tab: SettingsTab) -> &'static str {
    match tab {
        SettingsTab::General => "general",
        SettingsTab::InputAndPen => "input",
        SettingsTab::Themes => "themes",
        SettingsTab::Shortcuts => "shortcuts",
        SettingsTab::Performance => "performance",
    }
}

/// The pane's title, what it is for, and the way out.
///
/// Above the scroll area rather than inside it: the close mark is the only one
/// on the dialog and must not be scrollable away.
fn pane_header(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    ui.horizontal(|ui| {
        let (title, blurb) = match ed.ui.settings_tab {
            SettingsTab::General => (
                "General",
                "How the workspace itself behaves, before any document is open.",
            ),
            SettingsTab::InputAndPen => (
                "Input & pen",
                "Where a stroke's pressure comes from, and a live reading of what \
                 the window system is sending to judge it by.",
            ),
            SettingsTab::Themes => (
                "Themes",
                "The interface should disappear behind your work. Pick a theme.",
            ),
            SettingsTab::Shortcuts => (
                "Shortcuts",
                "Click any binding to rebind it. Conflicts are flagged, never \
                 silently dropped.",
            ),
            SettingsTab::Performance => ("", ""),
        };
        // Bounded, and told to wrap. A blurb is the longest run of text above
        // the fold, and a label in this horizontal layout would otherwise
        // extend — pushing the close mark off the edge on a narrow window and
        // widening the dialog on any other. `CLOSE_RESERVE` is what the mark
        // and its gap take.
        const CLOSE_RESERVE: f32 = 30.0;
        let text_width = (ui.available_width() - CLOSE_RESERVE).max(80.0);
        ui.vertical(|ui| {
            ui.set_max_width(text_width);
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
            ui.label(
                egui::RichText::new(title)
                    .size(15.0)
                    .color(p.text_strong)
                    .strong(),
            );
            controls::note(ui, p, blurb);
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
            let (rect, close) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
            icons::draw(
                ui.painter(),
                rect,
                Icon::Close,
                if close.hovered() {
                    p.text_strong
                } else {
                    p.text_dim
                },
            );
            if close.clicked() {
                ed.ui.settings_open = false;
            }
        });
    });
}

// ---------------------------------------------------------------------------
// General
// ---------------------------------------------------------------------------

fn general_pane(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor, actions: &mut UiActions) {
    controls::section(ui, p, "Interface");
    ui.scope(|ui| {
        ui.set_max_width(320.0);
        // egui's zoom factor is the single source of truth for this; keeping a
        // second copy alongside it is how the two end up disagreeing.
        let mut scale = ui.ctx().zoom_factor();
        if widgets::number_row(ui, p, &mut scale, scale_row()) {
            ui.ctx().set_zoom_factor(scale);
            prefs::mark_dirty();
        }
    });
    controls::note(
        ui,
        p,
        "Scales the panels and the type. The canvas is unaffected — brush sizes \
         are in document pixels, so painting looks the same at any scale.",
    );

    ui.add_space(16.0);
    controls::section(ui, p, "Start-up");
    controls::row(ui, p, "Show the splash screen", |ui| {
        // There *is* a splash, so the old wording here — "there is no splash to
        // skip" — was untrue. It is a progress overlay painted from the CPU
        // while the graphics driver starts, which is the only thing on screen
        // at that point; switching it off would leave an empty window instead.
        let _ = controls::text_button(ui, p, "Always", false, false).on_hover_text(
            "The splash is the start-up progress overlay, and it is only up while \
             the GPU is being set up. Turning it off would leave a blank window \
             for exactly as long, so there is nothing to switch.",
        );
    });

    ui.add_space(8.0);
    // The one control in Umber that governs a request leaving the machine, so
    // it is a live switch rather than a note — and it sits in General beside
    // the other "how the workspace behaves before a document is open" settings
    // rather than getting a pane of its own, which the design does not have.
    let mut check = ed.updates.check_on_startup;
    controls::row(ui, p, "Check for updates on start-up", |ui| {
        if widgets::toggle(ui, p, &mut check).clicked() {
            ed.updates.check_on_startup = check;
            // Changing this is also an answer to the first-run notice: somebody
            // who has found the switch has plainly been told.
            ed.updates.notice_seen = true;
            prefs::mark_dirty();
        }
    });
    controls::note(
        ui,
        p,
        "Asks GitHub which release is newest, once, when Umber starts. The \
         request carries nothing about you or your work. Umber does not sign \
         its releases — a download is checked against the size GitHub reports \
         and nothing stronger. Help, About has the details and a button to \
         check on demand.",
    );

    ui.add_space(16.0);
    controls::section(ui, p, "Documents");
    // The one setting that trades file size for a feature, so it is a switch
    // rather than a policy, and the note states the trade in megabytes rather
    // than in adverbs.
    let mut history = ed.ui.save_history;
    controls::row(ui, p, "Save the undo history in the document", |ui| {
        if widgets::toggle(ui, p, &mut history).clicked() {
            ed.ui.save_history = history;
            prefs::mark_dirty();
        }
    });
    controls::note(
        ui,
        p,
        "Lets a document be undone after it has been closed and reopened. The \
         history goes in as private entries other applications ignore, so the \
         file is still an ordinary OpenRaster. The newest edits are kept, up to \
         32 MB of them — a sketching session is well under a megabyte of that, \
         while an hour of full-canvas painting reaches the limit and its oldest \
         strokes are dropped. Switching this off makes saving quicker and the \
         file smaller, and costs only the history.",
    );

    ui.add_space(16.0);
    fonts_section(ui, p, ed);

    ui.add_space(16.0);
    undo_section(ui, p, ed);

    ui.add_space(16.0);
    autosave_section(ui, p, ed, actions);
}

/// Where the Text module looks for faces.
///
/// One folder and no list. Umber already reads every font installed on this
/// machine — see `umber_core::fonts` for why that is the feature rather than a
/// bundle — and this is the third source: a directory somebody keeps their own
/// faces in, for a foundry licence or a work library. Umber **reads** it and
/// copies nothing out of it, which the note says, because the moment it copied
/// a face it would be redistributing one inside somebody's own documents
/// folder.
///
/// The dialog blocks, which is what an explicit click may do; nothing on the
/// drawing path reaches it. Changing the folder goes through
/// [`prefs::set_font_folder`], which also throws the scan away — a library
/// still holding the old folder's faces would offer faces the artist has just
/// pointed Umber away from.
fn fonts_section(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    controls::section(ui, p, "Fonts");
    let current = ed
        .font_folder
        .as_ref()
        .map(|f| f.display().to_string())
        .unwrap_or_else(|| "None".to_string());
    controls::row(ui, p, "Extra font folder", |ui| {
        if ed.font_folder.is_some()
            && controls::text_button(ui, p, "Clear", false, true)
                .on_hover_text("Go back to this machine's own font directories alone")
                .clicked()
        {
            prefs::set_font_folder(ed, None);
            prefs::mark_dirty();
        }
        if controls::text_button(ui, p, "Choose…", false, true)
            .on_hover_text("Pick a directory of fonts to read beside the machine's own")
            .clicked()
            && let Some(folder) = rfd::FileDialog::new()
                .set_title("Choose a folder of fonts")
                .pick_folder()
        {
            prefs::set_font_folder(ed, Some(folder));
            prefs::mark_dirty();
        }
    });
    controls::note(
        ui,
        p,
        &format!(
            "Currently: {current}.\nEvery font installed on this machine is already \
             offered in the Text panel — this is for a folder of your own beside them, \
             such as a foundry licence or a work library. Umber reads it and copies \
             nothing out of it. TrueType and OpenType are read — .ttf, .otf, .ttc \
             and .otc — and web fonts are not."
        ),
    );
}

/// The Interface scale control's shape: a factor shown as a percentage, landing
/// on each 25%, handed back only when the drag ends.
///
/// Split out so the tests drive the control the dialog actually draws rather
/// than a copy of its numbers — the same reason [`crate::colorpicker`]'s angle
/// row is its own function.
///
/// Every figure anybody actually asks for here is on a quarter: 100% back to
/// where it started, 125% and 150% on a high-density screen. Landing on one by
/// dragging a rail 320 px wide across a range of 1.25 is a matter of luck, so
/// the rail snaps to each 25% and the figure beside it can be typed — somebody
/// who wants exactly 125% types 125.
pub(crate) fn scale_row() -> widgets::NumberRow<'static> {
    widgets::NumberRow {
        label: "Interface scale",
        range: prefs::MIN_SCALE..=prefs::MAX_SCALE,
        snap: 0.25,
        // The value is egui's zoom factor, a factor around 1; the readout is
        // the percentage everybody states a scale in.
        per_unit: 100.0,
        suffix: "%",
        decimals: 0,
        // The one deferred row in the application. This one is drawn inside the
        // thing it scales, so applying it per frame moves the track out from
        // under the pointer and the knob runs away from the hand holding it. A
        // *typed* figure is applied at once either way — the pointer is nowhere
        // near the track, so there is nothing to run away from.
        deferred: true,
    }
}

/// How much memory one document's undo history may hold.
///
/// A section of its own rather than a line under Documents: the setting above
/// is about what goes in a *file*, and this is about what a running session
/// holds. They are both "the undo history" and they trade different things.
fn undo_section(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    controls::section(ui, p, "Undo memory");

    ui.scope(|ui| {
        ui.set_max_width(320.0);
        // A ladder of doublings, like the autosave's expiry — the nearest rung
        // to whatever the history is actually holding itself to, so a
        // hand-edited figure between two rungs shows as the one it is closest
        // to rather than resetting the setting the moment the pane is drawn.
        let held = (ed.history.budget_bytes() / (1024 * 1024)) as u32;
        let nearest = prefs::UNDO_BUDGET_LADDER
            .iter()
            .enumerate()
            .min_by_key(|(_, mb)| mb.abs_diff(held))
            .map(|(i, _)| i)
            .unwrap_or(0);
        let mut step = nearest as f32;
        if widgets::slider_row(
            ui,
            p,
            "Keep up to",
            &mut step,
            0.0..=(prefs::UNDO_BUDGET_LADDER.len() - 1) as f32,
            false,
            |v| budget_label(prefs::UNDO_BUDGET_LADDER[budget_index(v)]),
        ) {
            let chosen = prefs::UNDO_BUDGET_LADDER[budget_index(step)];
            // The one door, so the document being edited and the one opened
            // next cannot end up on different limits — see `set_undo_budget`.
            prefs::set_undo_budget(ed, chosen);
            prefs::mark_dirty();
        }
    });
    controls::note(
        ui,
        p,
        "Per document, not per session: four tabs at 1 GB each is four \
         gigabytes of memory. How many steps that buys depends on the canvas, \
         because an entry holds the whole rectangle a stroke covered — a sketch \
         gets hundreds, while on a very large canvas a few broad strokes fill \
         any figure offered here and the oldest are dropped. More costs memory \
         the rest of the machine cannot then use; less costs how far back you \
         can go. The History panel says when it has started dropping edits.",
    );
}

/// A ladder step, clamped — a slider's value is a float and the ends can land a
/// hair outside. [`ladder_index`]'s counterpart for the undo budget.
fn budget_index(value: f32) -> usize {
    (value.round().max(0.0) as usize).min(prefs::UNDO_BUDGET_LADDER.len() - 1)
}

/// A budget as the dialog says it: megabytes up to a gigabyte, then gigabytes.
fn budget_label(megabytes: u32) -> String {
    if megabytes < 1024 {
        format!("{megabytes} MB")
    } else {
        let gb = megabytes as f32 / 1024.0;
        if (gb - gb.round()).abs() < 0.01 {
            format!("{} GB", gb.round() as u32)
        } else {
            format!("{gb:.1} GB")
        }
    }
}

/// Autosave: whether, how often, how long the internal copies are kept, and the
/// way to go and look at them.
fn autosave_section(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor, actions: &mut UiActions) {
    controls::section(ui, p, "Autosave");

    let mut on = ed.autosave.enabled;
    controls::row(ui, p, "Save open documents automatically", |ui| {
        if widgets::toggle(ui, p, &mut on).clicked() {
            ed.autosave.enabled = on;
            prefs::mark_dirty();
        }
    });
    controls::note(
        ui,
        p,
        "Waits for a gap between strokes, so it never interrupts one — “every \
         five minutes” means at the first quiet moment after five minutes. A \
         document you have saved somewhere is written back to that file, \
         replacing it without asking; one you have never saved goes only to \
         Umber's own folder.",
    );

    if !on {
        return;
    }

    ui.add_space(8.0);
    ui.scope(|ui| {
        ui.set_max_width(320.0);
        let mut minutes = (ed.autosave.interval.as_secs() / 60).max(1) as f32;
        if widgets::slider_row(
            ui,
            p,
            "How often",
            &mut minutes,
            autosave::MIN_INTERVAL_MINUTES as f32..=autosave::MAX_INTERVAL_MINUTES as f32,
            true,
            |v| {
                let m = v.round() as u32;
                if m == 1 {
                    "every minute".to_string()
                } else {
                    format!("every {m} minutes")
                }
            },
        ) {
            ed.autosave.interval = Duration::from_secs(minutes.round().max(1.0) as u64 * 60);
            prefs::mark_dirty();
        }

        ui.add_space(8.0);
        // A ladder rather than a free slider: the useful answers are a handful
        // of round durations, and nobody wants to land on exactly 30 days by
        // dragging. The preferences file still takes any number of hours.
        let hours = ed
            .autosave
            .expiry
            .map(|d| (d.as_secs() / 3600) as u32)
            .unwrap_or(0);
        let nearest = autosave::EXPIRY_LADDER
            .iter()
            .enumerate()
            .min_by_key(|(_, h)| h.abs_diff(hours))
            .map(|(i, _)| i)
            .unwrap_or(0);
        let mut step = nearest as f32;
        if widgets::slider_row(
            ui,
            p,
            "Keep Umber's own copies for",
            &mut step,
            0.0..=(autosave::EXPIRY_LADDER.len() - 1) as f32,
            false,
            |v| expiry_label(autosave::EXPIRY_LADDER[ladder_index(v)]),
        ) {
            let chosen = autosave::EXPIRY_LADDER[ladder_index(step)];
            ed.autosave.expiry = (chosen > 0).then(|| Duration::from_secs(chosen as u64 * 3600));
            prefs::mark_dirty();
        }
    });
    controls::note(
        ui,
        p,
        "Only Umber's own copies are ever deleted. Nothing here can reach a file \
         you chose the place for — a document saved to your own folder stays \
         there whatever this says.",
    );

    ui.add_space(8.0);
    let where_they_go = autosave::internal_dir_label();
    ui.horizontal(|ui| {
        ui.add(
            egui::Label::new(
                egui::RichText::new(format!("Copies are kept in {where_they_go}"))
                    .size(10.0)
                    .color(p.text_dim),
            )
            .truncate(),
        )
        .on_hover_text(where_they_go.as_str());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if controls::text_button(ui, p, "Open the folder", false, true)
                .on_hover_text("Show Umber's autosave folder in the file manager.")
                .clicked()
            {
                actions.reveal_autosaves = true;
            }
        });
    });
}

/// A ladder step, clamped — a slider's value is a float and the ends can land
/// a hair outside.
fn ladder_index(value: f32) -> usize {
    (value.round().max(0.0) as usize).min(autosave::EXPIRY_LADDER.len() - 1)
}

/// A number of hours as the dialog says it: for ever, in hours, or in days.
fn expiry_label(hours: u32) -> String {
    match hours {
        0 => "for ever".to_string(),
        1 => "1 hour".to_string(),
        h if h < 48 => format!("{h} hours"),
        h => {
            let days = h / 24;
            if days == 1 {
                "1 day".to_string()
            } else {
                format!("{days} days")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Input & pen
// ---------------------------------------------------------------------------

/// Height of the trace, and of the strip drawn under it.
const TRACE_HEIGHT: f32 = 92.0;
const STRIP_HEIGHT: f32 = 88.0;

/// Widest the test strip's mark gets, in points, at full pressure.
const NIB_MAX: f32 = 9.0;

/// Where pressure comes from, and a live reading of the pointer stream.
///
/// Everything reported here comes out of [`crate::inputlog`], which records the
/// events as they arrive at the window. Nothing on this page *asks* the pressure
/// model anything: the resolved figure is the one the real call answered, and
/// the strip runs on a copy. Reading a diagnostic must not be able to change
/// what it is reading — which is a different thing from setting the source,
/// where changing what is read is the whole point.
///
/// The order is what somebody testing a pen does, in the order they do it.
/// What is arriving comes first because it depends on no setting and answers
/// the first question — is the tablet driver in mouse mode, or is the pen
/// reaching Umber at all. The source picker comes next, because the three
/// figures under it are what it decides. Then the readings it produced, then
/// somewhere to draw, then the prose.
///
/// The prose stays at the foot as one block rather than a caption under each
/// reading: it is read once and the readings are watched, so keeping it apart
/// is what lets the instruments sit on screen together, and what falls below
/// the fold is then the part it does no harm to scroll to.
fn input_pane(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    // No scroll area of its own any more. This pane is taller than the dialog
    // and used to carry one, which is half of why the dialog changed size
    // between panes; `pane` now scrolls every page the same way.
    route_section(ui, p, ed);
    ui.add_space(12.0);
    source_section(ui, p, ed);
    ui.add_space(12.0);
    pressure_section(ui, p, ed);
    ui.add_space(12.0);
    strip_section(ui, p, ed);
    ui.add_space(16.0);
    guide_section(ui, p, ed);
}

/// The three names a pressure source goes by, in one place.
///
/// The picker and the line above the test strip used to carry a copy each, on
/// two different tabs, with a comment on the second asking that they be kept in
/// step. One table is the version that cannot drift.
const SOURCES: [(PressureSource, &str); 3] = [
    (PressureSource::Device, "Device"),
    (PressureSource::Simulated, "Speed"),
    (PressureSource::Constant, "Off"),
];

/// Which route the events are arriving by, and what the last one was.
///
/// The first question, and the one that separates "the tablet driver is in
/// mouse mode" from "the pen is reaching Umber and something later is wrong".
fn route_section(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    ui.horizontal(|ui| {
        controls::section(ui, p, "What is arriving");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if controls::text_button(ui, p, "Clear", false, true)
                .on_hover_text("Throw away everything recorded so far and start again.")
                .clicked()
            {
                ed.input.clear();
            }
        });
    });

    ui.horizontal(|ui| {
        widgets::chip(
            ui,
            p,
            "Mouse events",
            &ed.input.mouse_events.to_string(),
            "CursorMoved and MouseInput. A mouse sends these — and so does a pen \
             whose tablet driver is in mouse mode, which is the usual reason a \
             pen behaves like one.",
        );
        widgets::chip(
            ui,
            p,
            "Touch / pen events",
            &ed.input.touch_events.to_string(),
            "WindowEvent::Touch. On Windows a pen arrives here, through WM_POINTER, \
             and sends no mouse events at all. Zero of these while you draw with a \
             pen means the pen is not reaching Umber as a pen.",
        );
        widgets::chip(
            ui,
            p,
            "…carrying pressure",
            &ed.input.with_force.to_string(),
            "How many of those touches carried a force reading. Touches arriving \
             with none is a driver or platform limit, not something Umber can \
             work around.",
        );
    });

    ui.add_space(4.0);
    let last = ed.input.ring.newest();
    controls::row(ui, p, "Last event", |ui| {
        // Right to left, so the position goes on first and ends up on the far
        // right, reading "Touch / pen — hovering    812, 430 px". Omitted
        // entirely before the first event rather than drawn as a dash: there is
        // no position, and a placeholder beside "nothing yet" only reads as one
        // more thing that is missing.
        if let Some(s) = last {
            ui.label(
                egui::RichText::new(format!("{:.0}, {:.0} px", s.pos.x, s.pos.y))
                    .monospace()
                    .size(text::TINY)
                    .color(p.text_dim),
            );
            ui.add_space(12.0);
        }
        ui.label(
            egui::RichText::new(match last {
                Some(s) => format!("{} — {}", s.route.label(), s.motion.label()),
                None => "nothing yet".to_string(),
            })
            .size(text::SMALL)
            .color(if last.is_some() { p.text } else { p.text_dim }),
        );
    });

    // What the last press was *taken to mean*, which is a different question
    // from what arrived and the one that settles a whole class of pen report.
    // Three gestures — the Alt-drag brush resize, the Pan tool and the Zoom
    // tool — were once decided in the mouse arm of the event loop alone, so
    // under a pen they silently became a stroke; this row is where that shows.
    // Recorded by `gesture::press`'s one real call, never recomputed here.
    let last_gesture = ed.input.last_gesture();
    controls::row(ui, p, "Last press became", |ui| {
        ui.label(
            egui::RichText::new(match last_gesture {
                Some(g) => g.label(),
                None => "nothing yet",
            })
            .size(text::SMALL)
            .color(if last_gesture.is_some() {
                p.text
            } else {
                p.text_dim
            }),
        );
    });

    // Which cursor Umber asked for. "The arrow is still under my pen" has two
    // causes that look identical from the outside — Umber never asked, or Umber
    // asked and the window system did not carry it out — and this is the only
    // reading that separates them. What it reports is the request, not the
    // screen, and the wording says so: nothing in this process can see what
    // Windows actually drew.
    //
    // The count is doing the real work and the reading beside it is context.
    // This pane lives in a modal, and while one is open egui answers the
    // modal's own layer for every point in the window — so `pen_dot` declines
    // everywhere and every frame you can *read* this on says "the ordinary
    // pointer". `InputLog` therefore skips those frames, and the count is
    // clobber-proof besides: opening the menu to get here is an ordinary
    // `Area`, so those frames do count, and they record the pen over the menu.
    let cursor = ed.input.cursor_hidden;
    let asked = ed.input.hidden_frames;
    // A figure inside the sentence rather than a second row, so "it has asked
    // before" and "it is not asking now" are one reading instead of two that
    // have to be held side by side.
    let words = match (cursor, asked) {
        (_, 0) => "Never asked to hide it".to_string(),
        (Some(true), _) => "None, so the canvas can draw its own dot".to_string(),
        (_, n) => format!("The ordinary pointer now; hidden on {n} earlier frames"),
    };
    controls::row(ui, p, "Cursor asked for", |ui| {
        ui.label(
            egui::RichText::new(&words)
                .size(text::SMALL)
                .color(if asked == 0 { p.text_dim } else { p.text }),
        )
        .on_hover_text(CURSOR_HELP);
    });
}

/// Why the row above is worth reading, what to do to make it say something,
/// and what it cannot tell you.
///
/// Long, and its own constant so that it can be — this is the one control in
/// Umber whose whole purpose is to be read by somebody diagnosing hardware
/// nobody here has. The *previous* version was three lines and asserted that
/// "the ordinary pointer" meant Umber had decided the pen was not over the
/// canvas; that was false whenever this dialog was open, which is whenever
/// anybody could read it.
const CURSOR_HELP: &str = "What Umber asked the window system for, which is not \
     the same as what ended up on screen — nothing in this process can see what \
     was actually drawn. Frames with a dialog over the canvas are left out, \
     because Umber correctly asks for an ordinary pointer on those and this one \
     is a dialog. So: hover a pen over the canvas, then come back. If the count \
     is rising and an arrow is still showing under the pen, the request is being \
     dropped below Umber. If it stays at zero, Umber never asked — look at the \
     route and gesture rows above for why.";

/// Where pressure comes from: the one setting on this page, and the two knobs
/// that hang off one of its answers.
///
/// Above the readings rather than below them, because it is what they are
/// readings *of*. Somebody comparing the three sources flips the picker and
/// watches the meters, the trace and the strip underneath answer, which is the
/// gesture this page exists for and the one thing splitting it off onto a tab of
/// its own made impossible.
fn source_section(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    controls::section(ui, p, "Pressure source");

    let mut source = ed.pressure.source;
    ui.scope(|ui| {
        ui.set_max_width(320.0);
        if widgets::segmented(ui, p, &mut source, &SOURCES) {
            ed.pressure.source = source;
            prefs::mark_dirty();
        }
    });
    ui.add_space(6.0);

    match source {
        PressureSource::Device => controls::note(
            ui,
            p,
            "Touch screens report real pressure, and so do pens on Windows. Pens on \
             macOS and Linux do not reach Umber through the window system yet, so \
             there this behaves as Off — pick Speed for a stand-in. A mouse always \
             paints at full pressure.",
        ),
        PressureSource::Simulated => controls::note(
            ui,
            p,
            "Pressure is derived from how fast the stroke is moving: fast goes \
             thin, slow goes thick.",
        ),
        PressureSource::Constant => controls::note(ui, p, "Every sample paints at full pressure."),
    }

    // These two knobs feed only the speed model, so they appear with it.
    // Showing them permanently, greyed, would imply the device path has a
    // sensitivity to set, which it does not.
    if source == PressureSource::Simulated {
        ui.add_space(14.0);
        controls::section(ui, p, "Speed model");
        ui.scope(|ui| {
            ui.set_max_width(320.0);
            if widgets::slider_row(
                ui,
                p,
                "Speed for minimum pressure",
                &mut ed.pressure.max_speed,
                300.0..=8000.0,
                true,
                |v| format!("{v:.0} px/s"),
            ) {
                prefs::mark_dirty();
            }
            ui.add_space(8.0);
            if widgets::slider_row(
                ui,
                p,
                "Responsiveness",
                &mut ed.pressure.responsiveness,
                0.02..=1.0,
                false,
                |v| format!("{:.0}%", v * 100.0),
            ) {
                prefs::mark_dirty();
            }
        });
        controls::note(
            ui,
            p,
            "How quickly simulated pressure chases the speed. Lower is smoother \
             and lags further behind.",
        );
    }
}

/// The two pressure figures, and the trace of them.
///
/// Headed for what it shows rather than "Pressure", now that the section above
/// it is about pressure too — and "Reported and resolved" is the distinction
/// the two meters are here to draw.
fn pressure_section(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    controls::section(ui, p, "Reported and resolved");

    let last = ed.input.ring.newest();
    widgets::value_meter(
        ui,
        p,
        "Reported by the device",
        last.and_then(|s| s.reported),
        "none",
    );
    widgets::value_meter(
        ui,
        p,
        "Resolved by Umber",
        last.and_then(|s| s.resolved),
        "not resolved",
    );

    ui.add_space(8.0);
    widgets::pressure_graph(
        ui,
        p,
        TRACE_HEIGHT,
        inputlog::Ring::CAP,
        ed.input
            .ring
            .recent(inputlog::Ring::CAP)
            .map(|s| widgets::TracePoint {
                reported: s.reported,
                resolved: s.resolved,
            }),
    );
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        legend(ui, p, p.accent, "reported");
        ui.add_space(10.0);
        legend(ui, p, p.text_muted, "resolved");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(format!("last {} events", inputlog::Ring::CAP))
                    .size(text::TINY)
                    .color(p.text_dim),
            );
        });
    });
}

/// A short bar of colour and a name, for the trace's two lines.
fn legend(ui: &mut egui::Ui, p: &Palette, colour: Color32, label: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(14.0, 10.0), Sense::hover());
    ui.painter().rect_filled(
        Rect::from_center_size(rect.center(), vec2(14.0, 2.5)),
        1.25,
        colour,
    );
    ui.label(
        egui::RichText::new(label)
            .size(text::TINY)
            .color(p.text_muted),
    );
}

/// The scribble strip: somewhere to drag that draws the live pressure.
///
/// The source it is running under is still named on the same line, even though
/// the picker is now a few sections up: the strip behaves completely
/// differently on Device, Speed and Off, and the pane is long enough that the
/// picker can be scrolled off the top while somebody is scribbling. The button
/// that used to sit beside it has gone with the tab it led to.
fn strip_section(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    ui.horizontal(|ui| {
        controls::section(ui, p, "Try it");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(format!(
                    "pressure from {}",
                    source_label(ed.pressure.source)
                ))
                .size(text::TINY)
                .color(p.text_dim),
            );
        });
    });
    test_strip(ui, p, ed);
}

/// The name the picker gives a source, so the strip's line agrees with it.
fn source_label(source: PressureSource) -> &'static str {
    SOURCES
        .iter()
        .find(|(s, _)| *s == source)
        .map_or("", |(_, label)| label)
}

/// What all of the above means, and what is not on the page at all.
///
/// One block rather than a caption under each reading. The readings are watched
/// and this is read once, so keeping them apart is what lets all four
/// instruments sit on screen together — and the tilt statement belongs here for
/// the same reason it is a sentence rather than a meter: a tilt readout sitting
/// at zero would look like a device answering, when in fact nothing ever asks.
fn guide_section(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    controls::section(ui, p, "What to look for");

    controls::note(
        ui,
        p,
        "Hover a pen over the tablet without touching it. “Touch / pen — hovering” \
         means Umber can see it. If only the mouse counter moves, the driver is \
         sending the pen as a mouse and no pressure will ever arrive — that is a \
         tablet setting, not an Umber one.",
    );
    ui.add_space(6.0);
    controls::note(
        ui,
        p,
        "Reported and resolved are two different numbers and the difference is the \
         point. “Reported” is exactly what the device sent, and “none” means it \
         sent no reading at all — which the window system cannot tell apart from a \
         pen a hair off the glass, so Umber has to decide between them itself. \
         “Resolved” is what the brush was actually given, recorded from the one \
         real call rather than worked out again for the display.",
    );
    ui.add_space(6.0);
    controls::note(
        ui,
        p,
        "A gap in a line is a sample with no figure, never a zero. Draw a stroke \
         and lift off slowly: the reported line should slope down to nothing. If \
         it instead stops partway and the resolved line jumps back to the top, \
         pressure is not reaching zero and every stroke will end in a blob.",
    );
    ui.add_space(6.0);
    controls::note(
        ui,
        p,
        "The strip is its own picture. It goes through no document, no layer and \
         no undo history, and it resolves through a copy of the pressure model \
         reset on each press — asking the real one a second time would disturb the \
         stroke it is driving. On Speed the copy measures the strip's own pixels \
         rather than document pixels, so the threshold reads a little differently \
         here than on the canvas.",
    );

    ui.add_space(6.0);
    // Reported at all because a value that does arrive is worth showing, and the
    // only way anyone will ever find out that one does is by looking. iOS is the
    // single platform whose force is `Calibrated`, and the stylus altitude rides
    // inside that; Windows Ink sends `Normalized`, which has nowhere to carry
    // one.
    match ed.input.ring.newest().and_then(|s| s.altitude) {
        Some(radians) => {
            controls::row(ui, p, "Stylus altitude", |ui| {
                ui.label(
                    egui::RichText::new(format!("{:.1} degrees", radians.to_degrees()))
                        .monospace()
                        .size(text::TINY)
                        .color(p.text),
                );
            });
            controls::note(
                ui,
                p,
                "90 is upright. No brush setting follows tilt yet, so nothing is \
                 done with this — but it is arriving.",
            );
        }
        None => controls::note(
            ui,
            p,
            "There is no tilt reading. The only place winit has for one is the \
             stylus altitude inside a calibrated force, which is iOS's form; \
             Windows Ink sends a normalised force with nowhere to put an angle, \
             and macOS and Linux send no pen events at all. Tilt needs a native \
             tablet path, which is not built — so this says so rather than showing \
             a zero.",
        ),
    }
}

/// A strip to draw in, painted from what the pointer stream is doing.
///
/// Self-contained on purpose: it is its own picture, drawn straight out of the
/// sample ring. It reaches no document, no layer, no undo history and no GPU —
/// somebody testing a tablet should not have to open a document first, nor find
/// their canvas scribbled on afterwards.
fn test_strip(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), STRIP_HEIGHT),
        Sense::click_and_drag(),
    );
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, metrics::RADIUS_LARGE, p.backdrop);
    painter.rect_stroke(
        rect,
        metrics::RADIUS_LARGE,
        Stroke::new(
            1.0,
            if ed.input.probing() {
                p.accent
            } else {
                p.border
            },
        ),
        egui::StrokeKind::Inside,
    );

    // A press starts the strip's own model and a release ends it. Both from
    // egui's drag state rather than from the sample ring, because the ring
    // records what the *window* is doing and this has to be about this widget.
    let now = ed.now();
    if response.drag_started() || response.is_pointer_button_down_on() && !ed.input.probing() {
        let model = ed.pressure;
        ed.input.begin_probe(model, now);
    } else if !response.is_pointer_button_down_on() && ed.input.probing() {
        ed.input.end_probe();
    }

    if !ed.input.probing() && ed.input.probe_started == f64::MAX {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Drag here",
            FontId::proportional(text::SMALL),
            p.text_dim,
        );
        return;
    }

    // Only this drag: `begin_probe` stamps the moment it started, so the
    // previous scribble goes as soon as a new one begins rather than
    // accumulating into an unreadable tangle.
    let started = ed.input.probe_started;
    let scale = ed.pixels_per_point.max(1e-3);
    let mut previous: Option<(egui::Pos2, f32)> = None;
    for sample in ed.input.ring.iter() {
        if sample.at < started {
            continue;
        }
        // Resolved is what the strip is showing; reported is the fallback for a
        // sample nothing resolved, so a pen still draws something on the frame
        // its press was noticed.
        let Some(pressure) = sample.resolved.or(sample.reported) else {
            previous = None;
            continue;
        };
        let at = egui::pos2(sample.pos.x / scale, sample.pos.y / scale);
        let half = inputlog::nib_half_width(pressure, NIB_MAX);
        if let Some((from, from_half)) = previous {
            // A tapered quad between the two dots, so the width follows the
            // pressure along the segment rather than stepping at each sample.
            let along = at - from;
            let length = along.length();
            if length > 1e-3 {
                let across = egui::vec2(-along.y, along.x) / length;
                painter.add(egui::Shape::convex_polygon(
                    vec![
                        from + across * from_half,
                        at + across * half,
                        at - across * half,
                        from - across * from_half,
                    ],
                    p.text_strong,
                    Stroke::NONE,
                ));
            }
        }
        painter.circle_filled(at, half, p.text_strong);
        previous = Some((at, half));
    }
}

// ---------------------------------------------------------------------------
// Themes
// ---------------------------------------------------------------------------

/// What the Themes pane is holding between frames: the library, the fields
/// being typed into, and a Delete that has been pressed once.
///
/// In egui's temporary store rather than on [`Editor`] for the reason
/// `palettelib`'s state is: it is interaction state of one pane, nothing
/// outside this file has any use for it, and reading the library at launch for
/// somebody who has never opened this page would be a directory read — and
/// possibly a notice about a file that would not parse — for a feature they
/// have not asked for.
#[derive(Clone)]
struct Themes {
    /// The library, or why there is none. Two states rather than an `Option`,
    /// because "there is nowhere to keep themes on this system" is a sentence
    /// the controls have to be able to show instead of simply being dead —
    /// `brushlib::Store`'s and `palettelib::Store`'s arrangement.
    store: Result<std::sync::Arc<ThemeLibrary>, String>,
    /// Which theme [`Themes::hex`] and [`Themes::name`] were filled from, so a
    /// different theme being picked refills them.
    ///
    /// The empty string is a built-in, which has no fields.
    filled_from: String,
    /// One typed hex per token, in [`Token::ALL`] order.
    ///
    /// The buffer is the state's own and is edited in place: a `TextEdit`'s
    /// text belongs to the caller and the pane is rebuilt every frame, so a
    /// local copy would lose a character per frame.
    hex: Vec<String>,
    name: String,
    /// The id of the theme whose Delete has been pressed once. Deleting a theme
    /// cannot be undone — the history covers painting only — so it asks.
    confirming: Option<String>,
}

impl Themes {
    fn library(&self) -> Option<&std::sync::Arc<ThemeLibrary>> {
        self.store.as_ref().ok()
    }

    fn writable(&self) -> bool {
        self.store.is_ok()
    }

    /// The tooltip for a control that writes when there is nothing to write to.
    /// Never invented wording: it is what the library itself reported.
    fn why_not(&self) -> &str {
        match &self.store {
            Ok(_) => "",
            Err(why) => why,
        }
    }

    /// Refill the typed fields from the theme in hand, when it is not the one
    /// they already hold.
    fn refill(&mut self, ed: &Editor) {
        let id = ed.custom_theme.as_ref().map_or("", |t| t.id.as_str());
        if self.filled_from == id && self.hex.len() == Token::ALL.len() {
            return;
        }
        self.filled_from = id.to_owned();
        let palette = ed.palette();
        self.hex = Token::ALL
            .into_iter()
            .map(|token| themelib::hex(palette.token(token)))
            .collect();
        self.name = ed
            .custom_theme
            .as_ref()
            .map_or(String::new(), |t| t.name.clone());
        self.confirming = None;
    }
}

fn themes_id() -> egui::Id {
    egui::Id::new("settings-themes")
}

/// Read the pane's state back, reading the library off disk on the first frame.
fn load_themes(ctx: &egui::Context, ed: &mut Editor) -> Themes {
    let mut state = ctx
        .data(|d| d.get_temp::<Themes>(themes_id()))
        .unwrap_or_else(|| {
            let store = match ThemeLibrary::load() {
                Ok(library) => {
                    // A file that would not read means a theme somebody made is not
                    // in the row, which is worth one dialog on the first frame the
                    // pane is drawn. The editor's own notice rather than a strip of
                    // this page's, so there is one way a message reaches the user.
                    if !library.warnings().is_empty() {
                        ed.notice = Some(Notice {
                            title: "Some themes could not be read".to_owned(),
                            lines: library.warnings().to_vec(),
                        });
                    }
                    Ok(std::sync::Arc::new(library))
                }
                Err(e) => Err(e.to_string()),
            };
            Themes {
                store,
                filled_from: String::new(),
                hex: Vec::new(),
                name: String::new(),
                confirming: None,
            }
        });
    // The theme in hand may have gone — deleted in another window, or its file
    // removed between sessions — in which case `Editor::palette` is already
    // falling back to the built-in and the row must not draw it as selected.
    if let (Some(theme), Some(library)) = (&ed.custom_theme, state.library())
        && library.get(&theme.id).is_none()
    {
        ed.custom_theme = None;
        prefs::mark_dirty();
    }
    state.refill(ed);
    state
}

fn store_themes(ctx: &egui::Context, state: Themes) {
    ctx.data_mut(|d| d.insert_temp(themes_id(), state));
}

/// Put a library in the context before the pane is drawn, so that it reads that
/// one instead of the user's.
///
/// [`load_themes`] reads the directory only when there is no state yet — the
/// arrangement `palettelib` keeps — so seeding the state is the whole of it,
/// and no global has to be reachable from the drawing path.
///
/// It exists because two things that draw this pane must not read whatever
/// themes the machine happens to hold. **`docshot`** writes
/// `docs/images/settings-themes.png`, which is committed: a card, with its
/// name, for every theme the person regenerating it happens to have is a
/// contributor's own workspace published in the README — the leak
/// `prefs::set_config_path_label` already exists to stop, one door over. And
/// the pane's **measurements** would otherwise be taken against a card row of
/// a length nobody chose, so the same test would measure something different on
/// every machine.
pub(crate) fn stage_themes(ctx: &egui::Context, library: ThemeLibrary) {
    store_themes(
        ctx,
        Themes {
            store: Ok(std::sync::Arc::new(library)),
            filled_from: String::new(),
            hex: Vec::new(),
            name: String::new(),
            confirming: None,
        },
    );
}

/// Forget whatever the Themes pane was in the middle of — a Delete pressed
/// once, and anything typed into a field and not committed.
///
/// The library is deliberately *kept*: it is a directory read, and throwing it
/// away would re-read it every time somebody looked at another page. What goes
/// is the state that means "you are part way through something".
///
/// Called on every frame the pane is not in front, including every frame the
/// dialog is shut — so it returns as early as it can rather than taking egui's
/// data lock twice for nothing, exactly as [`stop_listening`] does. Emptying
/// `hex` is what makes [`Themes::refill`] run again on the way back in, since
/// its early return needs a full table.
fn forget_themes_edit(ctx: &egui::Context) {
    let Some(mut state) = ctx.data(|d| d.get_temp::<Themes>(themes_id())) else {
        return;
    };
    if state.confirming.is_none() && state.hex.is_empty() {
        return;
    }
    state.confirming = None;
    state.filled_from = String::new();
    state.hex.clear();
    state.name.clear();
    store_themes(ctx, state);
}

/// Run a write against the library and turn a failure into something the user
/// can read rather than a log line nobody sees.
///
/// Every [`ThemeLibrary`] write reaches the disk immediately — see
/// `themes_pane`'s note on why there is no Save button — so this is also where
/// "it did not get written" becomes visible. `None` means it did not happen.
fn write_theme<T>(
    state: &mut Themes,
    ed: &mut Editor,
    what: &str,
    op: impl FnOnce(&mut ThemeLibrary) -> Result<T, themelib::ThemeError>,
) -> Option<T> {
    let Ok(library) = &mut state.store else {
        return None;
    };
    match op(std::sync::Arc::make_mut(library)) {
        Ok(value) => Some(value),
        Err(e) => {
            ed.notice = Some(Notice {
                title: what.to_owned(),
                lines: vec![e.to_string()],
            });
            None
        }
    }
}

/// Put a theme from the library in hand, and keep the fallback beside it.
///
/// `UiState::theme` stays a *built-in*, always: it is what `Editor::palette`
/// falls back to when the file has gone, so it has to be the one this theme was
/// made from rather than left on whatever was showing before.
fn use_custom(ed: &mut Editor, theme: &themelib::CustomTheme) {
    ed.ui.theme = theme.base;
    ed.custom_theme = Some(theme.clone());
    prefs::mark_dirty();
}

fn themes_pane(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    let mut state = load_themes(ui.ctx(), ed);

    // Wrapped, because the row grows by a card for every theme somebody makes
    // and the pane is one fixed width — see the note on [`WIDTH`]. The dialog
    // butts its rail against its pane with no gutter, and that zero horizontal
    // spacing is inherited all the way down here, which left the cards
    // touching; set rather than left to the default, because the default is
    // whatever the enclosing layout last said.
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(CARD_GAP, CARD_GAP);
        let mut built_in = None;
        for kind in ThemeKind::ALL {
            let selected = ed.custom_theme.is_none() && ed.ui.theme == kind;
            // In the accent that is chosen, not the design's authored Umber:
            // the card's one coloured mark is the accent bar, and drawn from
            // `Palette::of` it advertised a colour the interface would not
            // show. The same argument `accent_choice`'s own swatches already
            // make for reading `accent.ink` rather than `Accent::swatch`.
            let swatch = Palette::with_accent(kind, ed.ui.accent);
            if theme_card(ui, p, &swatch, kind.label(), selected) {
                built_in = Some(kind);
            }
        }
        // Which card was clicked, applied after the row: putting a theme in
        // hand while the row is still being drawn would leave the cards after
        // it disagreeing with the ones before about which is in use. The rule
        // the layer panel's "All" box already follows — and it is also what
        // keeps this to one clone on a click rather than a copy of every theme
        // in the library on every frame.
        let mut chosen = None;
        if let Some(library) = state.library() {
            for (at, theme) in library.themes().iter().enumerate() {
                let selected = ed.custom_theme.as_ref().is_some_and(|t| t.id == theme.id);
                if theme_card(ui, p, &theme.palette, &theme.name, selected) {
                    chosen = Some(at);
                }
            }
        }
        let make = new_theme_card(ui, p, &state);

        if let Some(kind) = built_in {
            ed.ui.theme = kind;
            ed.custom_theme = None;
            prefs::mark_dirty();
            // The fields are filled from the theme in hand, and it has just
            // changed. `load_themes` refills at the top of the frame, so
            // without this the editor below would spend one frame showing the
            // colours of the theme that was in hand a moment ago.
            state.refill(ed);
        }
        if let Some(at) = chosen
            && let Some(theme) = state.library().and_then(|l| l.themes().get(at).cloned())
        {
            use_custom(ed, &theme);
            state.refill(ed);
        }
        if make {
            new_theme(&mut state, ed);
        }
    });

    ui.add_space(14.0);
    theme_editor(ui, p, ed, &mut state);

    store_themes(ui.ctx(), state);

    ui.add_space(16.0);
    ui.label(
        egui::RichText::new("Layout")
            .size(text::SMALL)
            .color(p.text_dim),
    );
    ui.add_space(6.0);
    // The left-handed mirror that used to live here is gone, and so is the tool
    // rail's own side setting that outlived it: the rail is a module now, and
    // every part of the workspace moves by being dragged in layout edit mode.
    ui.label(
        egui::RichText::new(
            "Every module, the tool rail included, is arranged by dragging it. \
             Turn on Window, Customise layout to move them; drop one at the edge \
             of a column to start a new column beside it. The same menu resets \
             the layout if it goes wrong.",
        )
        .size(10.0)
        .color(p.text_dim),
    );
}

/// A miniature of the workspace in that theme, so the choice is visual rather
/// than a name you have to try to remember the look of.
///
/// Takes the palette rather than a [`ThemeKind`], because a theme somebody made
/// is a palette and has no kind — and drawing both from one function is what
/// makes a custom theme's card the same card, rather than a second one that has
/// to be kept looking like this one.
fn theme_card(
    ui: &mut egui::Ui,
    p: &Palette,
    swatch: &Palette,
    name: &str,
    selected: bool,
) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(CARD[0], CARD[1]), Sense::click());

    let painter = ui.painter();
    let body = Rect::from_min_size(rect.min, vec2(rect.width(), 74.0));

    // Every band has to round wherever it meets the card's edge and stay square
    // everywhere else. egui clips rectangularly, so a rounded frame does not
    // trim what is painted inside it — a square band simply covers the corner,
    // and the theme's colour appears outside the card's outline.
    let r = metrics::RADIUS_LARGE as u8;
    let top = egui::CornerRadius {
        nw: r,
        ne: r,
        sw: 0,
        se: 0,
    };
    let bottom = egui::CornerRadius {
        nw: 0,
        ne: 0,
        sw: r,
        se: r,
    };
    let top_left = egui::CornerRadius {
        nw: r,
        ne: 0,
        sw: 0,
        se: 0,
    };
    let top_right = egui::CornerRadius {
        nw: 0,
        ne: r,
        sw: 0,
        se: 0,
    };

    painter.rect_filled(rect, metrics::RADIUS_LARGE, swatch.window);
    painter.rect_filled(body, top, swatch.backdrop);

    // Rail, canvas, dock — the design's three-band miniature.
    painter.rect_filled(
        Rect::from_min_size(body.left_top(), vec2(14.0, body.height())),
        top_left,
        swatch.chrome,
    );
    painter.rect_filled(
        Rect::from_min_size(
            egui::pos2(body.right() - 26.0, body.top()),
            vec2(26.0, body.height()),
        ),
        top_right,
        swatch.dock,
    );
    painter.rect_filled(
        Rect::from_center_size(body.center(), vec2(26.0, 26.0)),
        0.0,
        swatch.window,
    );
    painter.rect_filled(
        Rect::from_min_size(
            egui::pos2(body.right() - 22.0, body.top() + 10.0),
            vec2(14.0, 3.0),
        ),
        1.5,
        swatch.accent,
    );

    // Name strip along the foot, in the *theme's* chrome, so the card reads as
    // a sample of the theme rather than of the current one.
    let strip = Rect::from_min_size(
        egui::pos2(rect.left(), body.bottom()),
        vec2(rect.width(), rect.height() - body.height()),
    );
    painter.rect_filled(strip, bottom, swatch.chrome);
    // Truncated to the strip rather than allowed to run off it: a name is
    // something somebody typed, and a card whose label overran would paint over
    // the card beside it.
    let room = strip.width() - if selected { 56.0 } else { 20.0 };
    painter.text(
        strip.left_center() + vec2(10.0, 0.0),
        Align2::LEFT_CENTER,
        widgets::elide(painter, name, text::SMALL, room.max(20.0)),
        FontId::proportional(text::SMALL),
        swatch.text_strong,
    );
    if selected {
        painter.text(
            strip.right_center() - vec2(10.0, 0.0),
            Align2::RIGHT_CENTER,
            "in use",
            FontId::proportional(9.0),
            swatch.accent,
        );
    }

    painter.rect_stroke(
        rect,
        metrics::RADIUS_LARGE,
        Stroke::new(
            if selected { 2.0 } else { 1.0 },
            if selected { p.accent } else { p.border },
        ),
        egui::StrokeKind::Inside,
    );

    response.clicked()
}

/// The design's dashed "New theme" card.
///
/// Live: it copies the theme in front of the user into their own library. That
/// is what "new" has to mean here — a theme built from nothing is a palette of
/// transparent black, which is an interface nobody can see well enough to fix —
/// and it is what every application that has this feature means by it.
///
/// Returns whether it was clicked. Disabled — with the library's own wording,
/// never invented — where there is nowhere to write or no room left, because a
/// control that is live and then refuses is the one this project keeps
/// refusing.
fn new_theme_card(ui: &mut egui::Ui, p: &Palette, state: &Themes) -> bool {
    let room = state.library().is_some_and(|library| library.has_room());
    let live = state.writable() && room;
    let (rect, response) = ui.allocate_exact_size(
        vec2(CARD[0], CARD[1]),
        // A dead card still senses hover, because the tooltip explaining why it
        // is dead is the whole reason to draw it rather than hide it —
        // `controls::text_button`'s rule.
        if live { Sense::click() } else { Sense::hover() },
    );
    let painter = ui.painter();
    let dim = if live && response.hovered() {
        p.accent
    } else {
        p.border
    };

    // A dashed rounded rect: egui has no dash pattern, so the border is drawn
    // as short segments along each edge.
    let dash = |from: egui::Pos2, to: egui::Pos2| {
        let span = to - from;
        let len = span.length();
        let steps = (len / 8.0).round().max(1.0) as usize;
        for i in 0..steps {
            let a = from + span * (i as f32 / steps as f32);
            let b = from + span * ((i as f32 + 0.55) / steps as f32);
            painter.line_segment([a, b], Stroke::new(1.0, dim));
        }
    };
    dash(rect.left_top(), rect.right_top());
    dash(rect.right_top(), rect.right_bottom());
    dash(rect.right_bottom(), rect.left_bottom());
    dash(rect.left_bottom(), rect.left_top());

    let plus = Rect::from_center_size(rect.center() - vec2(0.0, 9.0), egui::Vec2::splat(16.0));
    controls::draw_glyph(painter, plus, Glyph::Plus, p.text_dim.gamma_multiply(0.5));
    painter.text(
        rect.center() + vec2(0.0, 14.0),
        Align2::CENTER_CENTER,
        "New theme",
        FontId::proportional(text::TINY),
        p.text_dim.gamma_multiply(0.5),
    );

    let tip = if live {
        "Copy the theme in use into your own library, and edit the copy"
    } else if !state.writable() {
        state.why_not()
    } else {
        "Your library already holds as many themes as Umber reads back"
    };
    response.on_hover_text(tip).clicked()
}

/// Copy the theme in front of the user into their library, and put it in hand.
fn new_theme(state: &mut Themes, ed: &mut Editor) {
    let from = ed
        .custom_theme
        .as_ref()
        .map_or_else(|| ed.ui.theme.label().to_owned(), |t| t.name.clone());
    let palette = ed.palette();
    let base = ed.ui.theme;
    let Some(id) = write_theme(state, ed, "Could not make a theme", |library| {
        library.duplicate(&from, base, palette)
    }) else {
        return;
    };
    let Some(made) = state
        .library()
        .and_then(|library| library.get(&id).cloned())
    else {
        return;
    };
    use_custom(ed, &made);
    // The fields are filled from the theme in hand, and the theme in hand has
    // just changed.
    state.refill(ed);
}

/// The design's theme editor.
///
/// A built-in is an *inspector*: every row is a real token out of the palette
/// in use, so it tells the truth about the running theme, and none of them can
/// be typed into, because a built-in is compiled into the binary and a change
/// written there would survive until the next release and then vanish —
/// `Library::collections`' argument. A theme out of the user's own library is
/// the same rows, editable.
///
/// **There is no Save button, and that is decided rather than overlooked.**
/// Every other control on this page writes itself out — `prefs::mark_dirty` and
/// `flush_if_idle` — and a `ThemeLibrary` write reaches the disk immediately,
/// as `PaletteLibrary`'s do. So a change here is already saved by the time a
/// Save could be clicked, and a button that did nothing new would be exactly
/// the control this project refuses everywhere else. The line under the heading
/// says so, because "where did my Save go" is a real question.
fn theme_editor(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor, state: &mut Themes) {
    Frame::NONE
        .fill(p.window)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(metrics::RADIUS_LARGE)
        .show(ui, |ui| {
            Frame::NONE
                .inner_margin(Margin::symmetric(14, 10))
                .show(ui, |ui| {
                    theme_editor_header(ui, p, ed, state);
                });

            let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
            ui.painter().rect_filled(line, 0.0, p.border);

            Frame::NONE
                .inner_margin(Margin::symmetric(14, 10))
                .show(ui, |ui| {
                    // Two columns, as the design lays them out. The gap goes
                    // *between* the pair and not after it, and the columns are
                    // sized from what is left over once it has been taken off —
                    // it used to be `(available - GAP) * 0.5` with a `GAP` after
                    // every row, so each line was one gap wider than the box it
                    // was in. The box grew, the pane grew, and the Themes page
                    // came out forty points wider than every other page of a
                    // dialog whose whole point is being one size.
                    let column = ((ui.available_width() - TOKEN_GAP) * 0.5).max(1.0);
                    let editable = ed.custom_theme.is_some();
                    for group in TokenGroup::ALL {
                        token_heading(ui, p, group.label());
                        for pair in group.tokens().chunks(2) {
                            ui.horizontal(|ui| {
                                // Stated rather than inherited. The gap between
                                // the columns is `TOKEN_GAP` and the columns are
                                // sized from what is left after it, so a line
                                // that also paid whatever horizontal spacing the
                                // enclosing layout happened to be using would be
                                // wider than the box by exactly that. It is zero
                                // in the dialog today — the rail butts against
                                // the pane — which is precisely the kind of
                                // thing that fits until somebody changes it
                                // three files away.
                                ui.spacing_mut().item_spacing.x = 0.0;
                                for (n, token) in pair.iter().enumerate() {
                                    if n > 0 {
                                        ui.add_space(TOKEN_GAP);
                                    }
                                    ui.scope(|ui| {
                                        ui.set_width(column);
                                        token_row(ui, p, ed, state, *token, editable);
                                    });
                                }
                            });
                        }
                        ui.add_space(6.0);
                    }

                    // Only for a built-in: the four accents are a shortcut for
                    // re-hueing a compiled-in palette, and a theme somebody made
                    // carries its accent as two of its own rows above. Drawing
                    // it here anyway would be a control that overwrote one of
                    // the colours they had just chosen — so it is not drawn,
                    // rather than drawn disabled.
                    if ed.custom_theme.is_none() {
                        accent_choice(ui, p, ed);
                    }
                });
        });
}

/// The editor's heading line: what is being edited, and what can be done with
/// it.
fn theme_editor_header(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor, state: &mut Themes) {
    ui.horizontal(|ui| {
        // The dialog sets the horizontal spacing to zero so the rail can butt
        // against the pane, and that zero is inherited all the way down here —
        // which is what left the Export and Save buttons touching. See
        // `metrics::BUTTON_GAP`.
        ui.spacing_mut().item_spacing.x = metrics::BUTTON_GAP;
        ui.label(
            egui::RichText::new("Theme editor")
                .size(text::CONTROL)
                .color(p.text_strong)
                .strong(),
        );

        // Collected and applied after the line, so the row has one writer per
        // frame: the buttons are drawn from what the theme was at the top of
        // the line, and a delete landing half way through would leave the rest
        // of the row naming a theme that had gone. The rule the layer panel's
        // "All" box already follows.
        let mut request: Option<Request> = None;
        // A rename is not one of those, and cannot be: clicking any of the
        // buttons takes the focus off the name field, so the two arrive in the
        // *same* frame. Collected separately and applied first, or a name typed
        // and then Exported would export under the name it had before — and a
        // name typed and then Deleted would spend the click on the rename and
        // need a second one.
        let mut renamed = false;

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = metrics::BUTTON_GAP;
            if let Some(theme) = &ed.custom_theme {
                let confirming = state.confirming.as_deref() == Some(theme.id.as_str());
                let label = if confirming { "Delete?" } else { "Delete" };
                if controls::text_button(ui, p, label, confirming, true)
                    .on_hover_text(if confirming {
                        "Click again to delete this theme. It cannot be undone — the \
                         history covers painting only."
                    } else {
                        "Delete this theme"
                    })
                    .clicked()
                {
                    request = Some(if confirming {
                        Request::Delete
                    } else {
                        Request::Confirm
                    });
                }
                if controls::text_button(ui, p, "Export…", false, true)
                    .on_hover_text("Write this theme out as a file you can keep or pass on")
                    .clicked()
                {
                    request = Some(Request::Export);
                }
            }
            let room = state.library().is_some_and(|library| library.has_room());
            let can_import = state.writable() && room;
            if controls::text_button(ui, p, "Import…", false, can_import)
                .on_hover_text(if can_import {
                    "Bring an .umbertheme file into your library"
                } else if !state.writable() {
                    state.why_not()
                } else {
                    "Your library already holds as many themes as Umber reads back"
                })
                .clicked()
            {
                request = Some(Request::Import);
            }

            // The name, filling whatever the buttons left. A real `TextEdit`,
            // so `ui::draw`'s one `set_typing` call already stops the canvas
            // hearing every keystroke — see the rule under Interface.
            match &ed.custom_theme {
                Some(_) => {
                    let width = NAME_FIELD.min(ui.available_width() - 16.0).max(40.0);
                    let field = inset_field(
                        ui,
                        p,
                        &mut state.name,
                        width,
                        FontId::proportional(text::SMALL),
                    );
                    // On losing focus rather than on every keystroke, unlike
                    // the colours: a rename moves the card in a list sorted by
                    // name, and a row that jumped under the pointer on every
                    // letter would be unusable. An emptied field is not a
                    // nameless theme — the model substitutes "Untitled theme".
                    //
                    // **Anything that takes the focus while this page is still
                    // drawn keeps what was typed; anything that takes the page
                    // away abandons it.** Clicking Export, or another field, is
                    // the first. Clicking another settings tab, or shutting the
                    // dialog — which is what Escape does here, since egui's
                    // `TextEdit` handles no `Key::Escape` and `egui::Modal`
                    // takes it — is the second: this row is simply not drawn
                    // again, `lost_focus` is never observed, and the next
                    // frame's `forget_themes_edit` empties the buffer. That is
                    // the abandon Escape looks like it should be, and leaving
                    // the buffer instead would be worse than either: reopening
                    // the page would show a name the theme does not have, whose
                    // next blur applied a rename nobody asked for twice.
                    renamed = field.lost_focus();
                }
                None => {
                    ui.label(
                        egui::RichText::new(format!(
                            "— {} is built in, so these are read-only",
                            ed.ui.theme.label()
                        ))
                        .size(10.0)
                        .color(p.text_dim),
                    );
                }
            }
        });

        if renamed {
            rename_theme(state, ed);
        }
        if let Some(request) = request {
            act_on(request, state, ed);
        }
    });

    controls::note(
        ui,
        p,
        if ed.custom_theme.is_some() {
            "Type a colour as six hex digits. Changes are saved as you make them, \
             in a file of their own — there is nothing to click."
        } else {
            "Pick New theme above to make a copy you can edit. The two that ship \
             with Umber are compiled into it, so a change written here would \
             vanish at the next update."
        },
    );
}

/// What the editor's heading line asked for. At most one per frame, since
/// acting on any of them changes what the rest of the line was drawn from.
enum Request {
    Confirm,
    Delete,
    Export,
    Import,
}

fn act_on(request: Request, state: &mut Themes, ed: &mut Editor) {
    match request {
        Request::Confirm => state.confirming = ed.custom_theme.as_ref().map(|t| t.id.clone()),
        Request::Delete => delete_theme(state, ed),
        Request::Export => export_theme(state, ed),
        Request::Import => import_theme(state, ed),
    }
}

fn delete_theme(state: &mut Themes, ed: &mut Editor) {
    let Some(id) = ed.custom_theme.as_ref().map(|t| t.id.clone()) else {
        return;
    };
    if write_theme(state, ed, "Could not delete the theme", |library| {
        library.remove(&id)
    })
    .is_none()
    {
        return;
    }
    // Back to the built-in it was made from, which is what `UiState::theme`
    // has been holding all along.
    ed.custom_theme = None;
    state.confirming = None;
    prefs::mark_dirty();
    state.refill(ed);
}

fn export_theme(state: &mut Themes, ed: &mut Editor) {
    let (Some(library), Some(theme)) = (state.library().cloned(), ed.custom_theme.clone()) else {
        return;
    };
    let Some(path) = rfd::FileDialog::new()
        .set_title("Export theme")
        .add_filter("Umber theme", &[themelib::EXTENSION])
        // The *id*, not the display name: an id is already a filename, and a
        // name may hold a separator or a colon, which is a file dialog opened
        // on a path nobody meant. `palettelib::export`'s rule.
        .set_file_name(format!("{}.{}", theme.id, themelib::EXTENSION))
        .save_file()
    else {
        return;
    };
    if let Err(e) = library.export(&theme.id, &path) {
        ed.notice = Some(Notice {
            title: "Could not export the theme".to_owned(),
            lines: vec![e.to_string()],
        });
    }
}

fn import_theme(state: &mut Themes, ed: &mut Editor) {
    let Some(paths) = rfd::FileDialog::new()
        .set_title("Import themes")
        .add_filter("Umber theme", &[themelib::EXTENSION])
        .pick_files()
    else {
        return;
    };
    // Written to directly rather than through `write_theme`, because that
    // raises a notice per failure and this loop has to end with **one**: a
    // folder of themes may hold a file that reads, one that lost lines and one
    // that will not open at all, and a notice per file that the next file
    // overwrites is the same as no notice for everything before it.
    let Ok(library) = &mut state.store else {
        ed.notice = Some(Notice {
            title: "Could not import".to_owned(),
            lines: vec![state.why_not().to_owned()],
        });
        return;
    };
    let library = std::sync::Arc::make_mut(library);
    let mut lines = Vec::new();
    let mut failed = false;
    let mut added = None;
    for path in &paths {
        match library.import(path) {
            Ok((id, skipped)) => {
                if skipped > 0 {
                    // An import that loses something must say so —
                    // `docimport`'s rule. A line Umber could not read is a
                    // colour that came out of the base theme instead.
                    lines.push(format!(
                        "{}: {skipped} line(s) could not be read, so those colours \
                         came from the theme it names as its base.",
                        path.display()
                    ));
                }
                added = Some(id);
            }
            Err(e) => {
                failed = true;
                // Named, because not every error carries the path — a library
                // that filled part way through a batch would otherwise repeat
                // one identical sentence with nothing to say which file it was
                // about.
                lines.push(format!("{}: {e}", path.display()));
            }
        }
    }
    let any = added.is_some();
    if let Some(id) = added
        && let Some(theme) = state
            .library()
            .and_then(|library| library.get(&id).cloned())
    {
        use_custom(ed, &theme);
        state.refill(ed);
    }
    if !lines.is_empty() {
        ed.notice = Some(Notice {
            // Both halves are reachable in one go, so the title says which of
            // the two happened rather than only the last — `palettelib`'s
            // wording and its reasoning.
            title: match (any, failed) {
                (true, true) => "Imported some, with notes".to_owned(),
                (true, false) => "Imported, with notes".to_owned(),
                (false, _) => "Could not import".to_owned(),
            },
            lines,
        });
    }
}

fn rename_theme(state: &mut Themes, ed: &mut Editor) {
    let Some(id) = ed.custom_theme.as_ref().map(|t| t.id.clone()) else {
        return;
    };
    let typed = state.name.trim().to_owned();
    if ed.custom_theme.as_ref().is_some_and(|t| t.name == typed) {
        return;
    }
    if write_theme(state, ed, "Could not rename the theme", |library| {
        library.rename(&id, &typed)
    })
    .is_none()
    {
        return;
    }
    // Back out of the library rather than out of what was typed: `rename`
    // numbers a name something else already has, and the field has to show what
    // was actually stored — otherwise it would sit there reading "Graphite"
    // beside a card labelled "Graphite 2".
    if let Some(stored) = state
        .library()
        .and_then(|library| library.get(&id).cloned())
    {
        state.name = stored.name.clone();
        ed.custom_theme = Some(stored);
    }
}

/// A heading over a group of tokens.
fn token_heading(ui: &mut egui::Ui, p: &Palette, title: &str) {
    ui.label(
        egui::RichText::new(title)
            .size(9.5)
            .color(p.text_dim.gamma_multiply(0.8))
            .strong(),
    );
}

/// One palette entry: a chip of the colour, its name, and its hex — typed into
/// where the theme is the user's own, read where it is built in.
fn token_row(
    ui: &mut egui::Ui,
    p: &Palette,
    ed: &mut Editor,
    state: &mut Themes,
    token: Token,
    editable: bool,
) {
    // The token's place in the buffer table. `None` is unreachable — every
    // token drawn comes out of `TokenGroup::tokens`, which is a filter over
    // `Token::ALL` — and it falls through to the read-only readout rather than
    // to slot zero, because writing *Backdrop's* buffer would be a row silently
    // editing the wrong colour, and to `expect` would be a panic on the drawing
    // path. Neither is a trade worth taking for a case that cannot happen.
    let at = Token::ALL.iter().position(|t| *t == token);
    let colour = ed.palette().token(token);
    ui.horizontal(|ui| {
        let (chip, _) = ui.allocate_exact_size(egui::Vec2::splat(18.0), Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(chip, 4.0, colour);
        painter.rect_stroke(
            chip,
            4.0,
            Stroke::new(1.0, p.popover_border),
            egui::StrokeKind::Inside,
        );
        ui.add_space(metrics::BUTTON_GAP);
        ui.label(
            egui::RichText::new(token.label())
                .size(text::SMALL)
                .color(p.text_muted),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // The length check is a third line of defence, after `refill`
            // always filling the whole table and `Token::ALL` being what this
            // loop walks — it is here because the alternative failure is an
            // index panic on the *drawing path*, which is the worst place in
            // the application to put one. `Palette::link_colour`'s modulo is
            // the same argument.
            let Some(at) = at.filter(|_| editable && state.hex.len() == Token::ALL.len()) else {
                ui.label(
                    egui::RichText::new(themelib::hex(colour))
                        .monospace()
                        .size(text::TINY)
                        .color(p.text),
                );
                return;
            };
            let field = inset_field(
                ui,
                p,
                &mut state.hex[at],
                HEX_FIELD,
                FontId::monospace(text::TINY),
            );
            // Applied live once six digits are in, and on losing focus for any
            // form the parser takes. Both halves are wanted and neither alone
            // does: applying on every keystroke would paint the interface in
            // `#CC0088` on the way to `#C08A4E`, because three digits are a
            // legal short hex; applying only on blur would mean a colour cannot
            // be judged against the interface it is for while it is being
            // typed, which is the whole point of a theme editor.
            let body = state.hex[at].trim().trim_start_matches('#');
            if field.changed() && body.len() == 6 {
                set_token(ed, state, token, at);
            }
            // On the way out, a field that will not read goes back to the
            // colour that is actually there. While it has the caret it is what
            // somebody is typing and must be left alone; once it does not, it
            // is a *readout*, and a readout saying `rebeccapurple` beside a
            // chip that is `#111214` is the control that lies.
            if field.lost_focus() && !set_token(ed, state, token, at) {
                state.hex[at] = themelib::hex(colour);
            }
        });
    });
}

/// Put what was typed into the theme in hand, and write it out. Answers whether
/// it read as a colour at all.
fn set_token(ed: &mut Editor, state: &mut Themes, token: Token, at: usize) -> bool {
    let Some(colour) = themelib::parse_hex(&state.hex[at]) else {
        // Nothing is applied and nothing is refused: while the field has the
        // caret it keeps what was typed so it can be corrected, and the palette
        // keeps the colour it had. A theme that quietly took black for a
        // misread line would be a theme with an invisible interface in it. The
        // caller is what puts the readout back on the way out.
        return false;
    };
    let Some(theme) = ed.custom_theme.as_mut() else {
        return false;
    };
    if theme.palette.token(token) == colour {
        // The blur after a keystroke that already landed. Writing the file
        // again would be a second write for no change.
        state.hex[at] = themelib::hex(colour);
        return true;
    }
    theme.palette.set_token(token, colour);
    // Normalised back into the field, so `#fff` becomes `#FFFFFF` once it has
    // been taken — which is also what says it was taken.
    state.hex[at] = themelib::hex(colour);
    let stored = theme.clone();
    write_theme(state, ed, "Could not save the theme", |library| {
        library.save(stored)
    });
    true
}

/// The design's four accent options.
///
/// Live: `Palette::with_accent` re-hues a theme without duplicating it, so this
/// is one field rather than four more palettes to keep in step.
fn accent_choice(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Accent")
                .size(text::SMALL)
                .color(p.text_muted),
        );
        ui.add_space(6.0);
        for accent in Accent::ALL {
            let (rect, response) = ui.allocate_exact_size(egui::Vec2::splat(18.0), Sense::click());
            let chosen = accent == ed.ui.accent;
            let painter = ui.painter();
            // The swatch shows the accent as it will look on the theme being
            // used, not the design's dark-theme value: on Paper the accents are
            // darkened, and a swatch that ignored that would advertise a colour
            // the interface never shows.
            painter.circle_filled(rect.center(), 8.0, accent.ink(ed.ui.theme));
            if chosen {
                painter.circle_stroke(rect.center(), 10.0, Stroke::new(1.5, p.text_strong));
            } else if response.hovered() {
                painter.circle_stroke(rect.center(), 10.0, Stroke::new(1.0, p.text_dim));
            }
            if response.clicked() && ed.ui.accent != accent {
                ed.ui.accent = accent;
                prefs::mark_dirty();
            }
            let _ = response.on_hover_text(accent.label());
        }
    });
}

// ---------------------------------------------------------------------------
// Shortcuts
// ---------------------------------------------------------------------------

/// What the shortcuts pane is in the middle of.
///
/// Kept in egui's temporary store rather than in `Editor` because it is
/// interaction state of one dialog, not state of the document or the workspace:
/// nothing outside this file has any use for it, and it should not survive the
/// dialog being closed.
#[derive(Clone, Default)]
struct Editing {
    /// The binding currently listening for a key: which action, and which of
    /// its bindings — `None` for a new one being added.
    listening: Option<(Action, Option<usize>)>,
    /// Why the last key press was refused. Cleared by the next one.
    refused: Option<&'static str>,
    /// The design's action search.
    query: String,
}

/// What a row asked for this frame. At most one, since acting on it rebuilds
/// the table the rest of the loop is reading.
enum RowRequest {
    /// Listen for a chord for the nth binding, or for a new one.
    Listen(Option<usize>),
    /// Drop the nth binding.
    Clear(usize),
    /// Put this action's factory bindings back.
    Reset,
}

fn editing_id() -> egui::Id {
    egui::Id::new("settings-shortcut-editing")
}

fn store_editing(ctx: &egui::Context, editing: Editing) {
    // Dispatch to the canvas is suspended for exactly as long as a field is
    // listening — otherwise pressing B to bind it would also select the brush,
    // and Ctrl+Z would undo the last stroke on the way past.
    shortcuts::set_capturing(editing.listening.is_some());
    ctx.data_mut(|d| d.insert_temp(editing_id(), editing));
}

/// Abandon whatever the shortcuts pane was in the middle of.
///
/// Called on every frame the pane is not in front, including every frame the
/// dialog is shut — so it returns as early as it can rather than taking egui's
/// data lock twice for nothing.
fn stop_listening(ctx: &egui::Context) {
    let Some(editing) = ctx.data(|d| d.get_temp::<Editing>(editing_id())) else {
        return;
    };
    if editing.listening.is_some() {
        shortcuts::set_capturing(false);
    }
    ctx.data_mut(|d| d.remove::<Editing>(editing_id()));
}

fn shortcuts_pane(ui: &mut egui::Ui, p: &Palette) {
    let mut bindings = shortcuts::published();
    let mut editing = ui
        .ctx()
        .data_mut(|d| d.get_temp::<Editing>(editing_id()).unwrap_or_default());
    let mut changed = false;

    // 1. Anything the user typed since the last frame.
    if let Some((action, nth)) = editing.listening {
        let at = nth.and_then(|n| shortcuts::slot_of(&bindings, action, n));
        match controls::capture(ui) {
            Some(Captured::Cancelled) => {
                editing.listening = None;
                editing.refused = None;
            }
            Some(Captured::Rejected(why)) => editing.refused = Some(why),
            Some(Captured::Chord(chord)) => {
                editing.listening = None;
                editing.refused = None;
                // Whatever else holds the chord keeps it. The clash is drawn on
                // both rows instead — flagged, never silently dropped.
                shortcuts::bind(&mut bindings, action, at, chord);
                changed = true;
            }
            None => {}
        }
    }

    // 2. Header row: search, and the preset controls the design shows.
    ui.horizontal(|ui| {
        ui.scope(|ui| {
            ui.set_width(300.0);
            controls::search_field(ui, p, &mut editing.query, "Search actions");
        });
        // Right to left, so Export goes on first to end up on the right —
        // reading Import then Export, as the design has them.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let _ = controls::text_button(ui, p, "Export", false, false).on_hover_text(
                "Your bindings are already kept in the preferences file named at the \
                 foot of this dialog, which is plain text and can be copied.",
            );
            let _ = controls::text_button(ui, p, "Import", false, false)
                .on_hover_text("Reading a keymap needs a file format; there is none yet.");
        });
    });
    if let Some(why) = editing.refused {
        ui.add_space(4.0);
        controls::banner(ui, p, why, |_| {});
    }
    ui.add_space(8.0);

    // 3. The list.
    let query = editing.query.trim().to_lowercase();
    let mut request: Option<(Action, RowRequest)> = None;
    // The list is drawn whole, inside the pane's own scroll area, rather than
    // scrolling within itself. This is the longest content in the dialog and it
    // used to carry a nested scroll area sized from the space left over — which
    // is a second thing deciding how tall the dialog is, and a wheel that means
    // two different things depending on which pixel it is over. One scroll area
    // per dialog; the frame here is a border round the rows and nothing more.
    Frame::NONE
        .fill(p.window)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(metrics::RADIUS_LARGE)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let mut current = "";
            let mut any = false;
            for action in Action::ALL {
                if !query.is_empty()
                    && !action.label().to_lowercase().contains(&query)
                    && !action.category().to_lowercase().contains(&query)
                {
                    continue;
                }
                any = true;
                let category = action.category();
                if category != current {
                    current = category;
                    category_heading(ui, p, category);
                }
                if let Some(row) = shortcut_row(ui, p, &bindings, action, &editing) {
                    request = Some((action, row));
                }
            }
            if !any {
                Frame::NONE
                    .inner_margin(Margin::symmetric(14, 12))
                    .show(ui, |ui| {
                        controls::note(ui, p, "No command matches that.");
                    });
            }
        });

    // 4. Whatever the list asked for.
    if let Some((action, row)) = request {
        match row {
            RowRequest::Listen(nth) => {
                editing.listening = Some((action, nth));
                editing.refused = None;
            }
            RowRequest::Clear(nth) => {
                if let Some(index) = shortcuts::slot_of(&bindings, action, nth) {
                    shortcuts::remove(&mut bindings, index);
                    changed = true;
                }
            }
            RowRequest::Reset => {
                shortcuts::reset_action(&mut bindings, action);
                changed = true;
            }
        }
    }

    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let clashes = shortcuts::shadowed(&bindings).len();
        controls::note(
            ui,
            p,
            &if clashes == 0 {
                "Space and Escape cannot be bound: Space pans while you draw, and \
                 Escape is what cancels a rebind."
                    .to_string()
            } else if clashes == 1 {
                "One key does two things. The command listed first wins.".to_string()
            } else {
                format!("{clashes} keys each do two things. The command listed first wins.")
            },
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let all_default = Action::ALL
                .into_iter()
                .all(|a| shortcuts::is_default(&bindings, a));
            if controls::text_button(ui, p, "Restore defaults", false, !all_default)
                .on_hover_text(if all_default {
                    "Every shortcut is already at its default."
                } else {
                    "Put every shortcut back the way it shipped."
                })
                .clicked()
            {
                bindings = shortcuts::defaults();
                changed = true;
            }
        });
    });

    if changed {
        shortcuts::publish(bindings);
        prefs::mark_dirty();
    }
    store_editing(ui.ctx(), editing);
}

/// The design's section rule: small, wide-tracked, upper case.
///
/// egui cannot letter-space a run of text, so the tracking is done by hand —
/// the alternative, dropping it, loses the one thing that separates a heading
/// from a very dim row.
fn category_heading(ui: &mut egui::Ui, p: &Palette, name: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
    let font = FontId::proportional(10.0);
    let painter = ui.painter();
    let colour = p.text_dim.gamma_multiply(0.75);

    let mut x = rect.left() + 14.0;
    for ch in name.to_uppercase().chars() {
        let glyph = ch.to_string();
        let width = painter
            .layout_no_wrap(glyph.clone(), font.clone(), colour)
            .size()
            .x;
        painter.text(
            egui::pos2(x, rect.center().y),
            Align2::LEFT_CENTER,
            glyph,
            font.clone(),
            colour,
        );
        x += width + 2.0;
    }

    painter.rect_filled(
        Rect::from_min_size(rect.left_bottom(), vec2(rect.width(), 1.0)),
        0.0,
        p.border.gamma_multiply(0.7),
    );
}

/// One action: its name, any clash it has, and the keys bound to it.
fn shortcut_row(
    ui: &mut egui::Ui,
    p: &Palette,
    bindings: &[Binding],
    action: Action,
    editing: &Editing,
) -> Option<RowRequest> {
    let chords = shortcuts::chords_for(bindings, action);
    let listening = |nth: Option<usize>| editing.listening == Some((action, nth));
    let armed = editing.listening.is_some_and(|(a, _)| a == action);
    let mut request = None;

    // The row's rectangle is claimed before anything is drawn into it, because
    // its own hover state decides what goes in — inferring hover from the `Ui`
    // afterwards would report every row below the pointer as hovered, since an
    // unfinished `Ui` still owns the rest of the column.
    const ROW_HEIGHT: f32 = 34.0;
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::hover());
    // `contains_pointer`, never `hovered`. egui stops the hover search at the
    // topmost *interactive* widget under the pointer, so a `Sense::hover()`
    // rectangle allocated before a button inside it reads as not-hovered the
    // moment the pointer is over that button. The buttons below exist only
    // while the row is lit, so `hovered` would make the answer depend on
    // itself: lit, so the `+` is allocated; over the `+`, so not lit; not lit,
    // so no `+`; over the row, so lit — a one-frame oscillation.
    //
    // `contains_pointer` is decided by geometry alone — is the pointer inside
    // this rectangle, in this layer — and the rectangle is allocated
    // unconditionally at a fixed height *before* anything is put in it. So
    // nothing drawn as a consequence of the answer can change the answer. That
    // is the property to keep: taking the union of the row's and the buttons'
    // responses only damps the loop, because the buttons still have to exist
    // to be asked, and testing a response that is itself contested leaves the
    // feedback path in place.
    let hovered = response.contains_pointer();

    let painter = ui.painter();
    if armed {
        painter.rect_filled(rect, 0.0, p.control_active);
    } else if hovered {
        painter.rect_filled(rect, 0.0, p.control);
    }
    // A hairline under every row, as the design has it.
    painter.rect_filled(
        Rect::from_min_size(rect.left_bottom() - vec2(0.0, 1.0), vec2(rect.width(), 1.0)),
        0.0,
        p.border.gamma_multiply(0.7),
    );

    let inner = rect.shrink2(vec2(14.0, 7.0));
    let mut ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    let ui = &mut ui;

    ui.label(
        egui::RichText::new(action.label())
            .size(text::CONTROL)
            .color(if armed { p.text_strong } else { p.text }),
    );

    // Clashes are named on the row that has them; a clash reported anywhere
    // else is not something the reader can act on.
    let clashing: Vec<&str> = (0..chords.len())
        .filter_map(|n| shortcuts::slot_of(bindings, action, n))
        .flat_map(|i| shortcuts::clashes_with(bindings, i))
        .map(|a| a.label())
        .collect();
    if let Some(first) = clashing.first() {
        controls::conflict_badge(ui, p, &format!("also {first}"));
    }

    // A new binding is captured at the end of the row, where it will land; an
    // action with nothing bound has no cap to replace, so its hint goes there
    // too.
    let hint_at_end = listening(None) || (armed && chords.is_empty());

    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if hint_at_end {
            controls::capture_hint(ui, p);
        }

        // The design keeps the resting row clean, so the editing buttons appear
        // on hover. Their width is reserved either way, or every keycap would
        // jump sideways under the pointer at the moment the row lit up.
        if hovered {
            let is_default = shortcuts::is_default(bindings, action);
            if controls::icon_button(
                ui,
                p,
                Glyph::Revert,
                !is_default,
                if is_default {
                    "Already the default."
                } else {
                    "Restore the default for this command."
                },
            )
            .clicked()
            {
                request = Some(RowRequest::Reset);
            }
            if controls::icon_button(
                ui,
                p,
                Glyph::Plus,
                true,
                "Add another key for this command.",
            )
            .clicked()
            {
                request = Some(RowRequest::Listen(None));
            }
        } else {
            ui.add_space(46.0);
        }

        if chords.is_empty() {
            if !armed && controls::keycap(ui, p, "not bound", CapState::Unbound, false).clicked {
                request = Some(RowRequest::Listen(Some(0)));
            }
            return;
        }

        // Right-to-left layout places what is added first furthest right, so
        // the caps go on in reverse to read in table order.
        for (nth, chord) in chords.iter().enumerate().rev() {
            if listening(Some(nth)) {
                controls::capture_hint(ui, p);
                continue;
            }
            let clashes = shortcuts::slot_of(bindings, action, nth)
                .is_some_and(|i| !shortcuts::clashes_with(bindings, i).is_empty());
            let state = if clashes {
                CapState::Clashing
            } else {
                CapState::Bound
            };
            let cap = controls::keycap(ui, p, &chord.display(), state, true);
            if cap.cleared {
                request = Some(RowRequest::Clear(nth));
            } else if cap.clicked {
                request = Some(RowRequest::Listen(Some(nth)));
            }
        }
    });

    request
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

fn storage_footer(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    // Its own vertical spacing, so what it costs is the three parts
    // `FOOTER_RESERVE` adds up and not those plus whatever gap the enclosing
    // layout happened to be using.
    ui.spacing_mut().item_spacing.y = 0.0;
    let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(line, 0.0, p.border);
    ui.add_space(FOOTER_GAP);

    let path = prefs::config_path_label();
    ui.horizontal(|ui| {
        ui.add(
            egui::Label::new(
                egui::RichText::new(format!("Saved to {path}"))
                    .size(10.0)
                    .color(p.text_dim),
            )
            .truncate(),
        )
        .on_hover_text(path.as_str());

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if controls::text_button(ui, p, "Restore all settings", false, true)
                .on_hover_text("Put every setting on every tab back to its default.")
                .clicked()
            {
                let defaults = prefs::Prefs::default();
                prefs::apply(&defaults, ui.ctx(), ed);
                stop_listening(ui.ctx());
                prefs::save(&defaults);
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Editor;
    use crate::theme::ThemeKind;
    use egui::{Rect, pos2};

    /// The panes and the rail, measured in the rectangles the dialog hands
    /// them.
    ///
    /// CPU tests, because this is geometry and needs no device — the preview
    /// below is what says whether the result *looks* right, and these are what
    /// fail the build when it stops being true. Same idiom as
    /// `panels`' `ticking_a_layer_does_not_move_the_layer_list`.
    fn measure(tab: Option<SettingsTab>) -> egui::Vec2 {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(WIDTH, HEIGHT))),
            ..Default::default()
        };
        let palette = Palette::of(ThemeKind::Graphite);
        let mut ed = Editor::default();
        if let Some(tab) = tab {
            ed.ui.settings_tab = tab;
        }
        // An empty library rather than whatever themes this machine holds. The
        // Themes pane's card row grows by a card per theme, so without this the
        // same measurement is a different measurement on every machine — and it
        // would read the tester's own data directory to take it.
        stage_themes(&ctx, ThemeLibrary::default());
        let given = match tab {
            Some(_) => vec2(WIDTH - RAIL_WIDTH, HEIGHT),
            None => vec2(RAIL_WIDTH, HEIGHT),
        };
        // Three passes and the last is the one read: the first through a fresh
        // context builds the font atlas, and text laid out against a half-built
        // one is not the size it will settle at.
        let mut measured = egui::Vec2::ZERO;
        for _ in 0..3 {
            let _ = ctx.run_ui(input.clone(), |ui| {
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(Rect::from_min_size(pos2(0.0, 0.0), given))
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                match tab {
                    Some(_) => {
                        let mut actions = crate::ui::UiActions::default();
                        pane(&mut child, &palette, &mut ed, &mut actions);
                    }
                    None => rail(&mut child, &palette, &mut ed),
                }
                measured = child.min_rect().size();
            });
        }
        measured
    }

    /// Every pane is the width the dialog handed it, so the dialog is one size
    /// whatever page is in front.
    ///
    /// This was a real bug and the Themes page was the one with it: its token
    /// columns were `(available - GAP) * 0.5` with a `GAP` added after *every*
    /// one instead of between the two, so each line was one gap wider than the
    /// box it was in — and the box, the pane and the modal all grew with it.
    /// The page came out forty points wider than the other three, in a dialog
    /// whose whole point is not changing size as you move between them.
    #[test]
    fn every_settings_pane_is_the_width_it_was_given() {
        let given = WIDTH - RAIL_WIDTH;
        for tab in [
            SettingsTab::General,
            SettingsTab::InputAndPen,
            SettingsTab::Themes,
            SettingsTab::Shortcuts,
        ] {
            let width = measure(Some(tab)).x;
            assert!(
                width <= given,
                "the {tab:?} pane reported {width} points in a {given}-point column",
            );
        }
    }

    /// The pane draws whatever library it was seeded with, and reads the disk
    /// only when it was seeded with none.
    ///
    /// This is the whole of how `docshot` keeps a contributor's own themes out
    /// of a committed README picture, and how the two measurements above stop
    /// depending on the machine running them. It is checked here rather than
    /// left to the preview, because the preview wants a GPU and this does not.
    #[test]
    fn a_seeded_library_is_what_the_pane_reads() {
        let ctx = egui::Context::default();
        let dir = std::env::temp_dir().join(format!("umber-themes-seed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut library = ThemeLibrary::load_from(&dir);
        library
            .duplicate(
                "Seeded",
                ThemeKind::Graphite,
                Palette::of(ThemeKind::Graphite),
            )
            .expect("a fresh directory");
        stage_themes(&ctx, library);

        let mut ed = Editor::default();
        let state = load_themes(&ctx, &mut ed);
        let names: Vec<&str> = state
            .library()
            .expect("seeded")
            .themes()
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        assert_eq!(names, ["Seeded"], "the pane read a library nobody seeded");

        // And an empty one is empty, which is what `docshot` seeds: whatever
        // the machine's own directory holds must not reach the picture.
        stage_themes(&ctx, ThemeLibrary::default());
        let mut ed = Editor::default();
        let state = load_themes(&ctx, &mut ed);
        assert!(
            state.library().expect("seeded").themes().is_empty(),
            "an empty seed still let the user's directory through"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What the footer costs must fit the room the pane keeps for it.
    ///
    /// This is the assertion that actually pins the fix, and the one below is
    /// not: `pane` now allocates exactly the height it was handed and draws the
    /// footer into a `new_child`, which never advances the parent — so the
    /// pane's own height can no longer fail whatever the footer turns out to
    /// cost. What *would* fail instead is silent: the footer paints outside its
    /// rectangle, and nothing clips it. So the footer is measured on its own,
    /// in a rectangle of its own, against the reserve.
    #[test]
    fn the_footer_fits_the_room_the_pane_keeps_for_it() {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(WIDTH, HEIGHT))),
            ..Default::default()
        };
        let palette = Palette::of(ThemeKind::Graphite);
        let mut ed = Editor::default();
        let mut measured = 0.0;
        for _ in 0..3 {
            let _ = ctx.run_ui(input.clone(), |ui| {
                // The width the *pane* hands it, which is the column less the
                // pane's own margin. Measured in the bare column the footer had
                // 56 points it does not have, and the path label is
                // `.truncate()`d rather than wrapped — so the day that label
                // wraps instead, a test taking the wider figure would stay
                // green while the footer overflowed.
                let inner = WIDTH - RAIL_WIDTH - PANE_MARGIN as f32 * 2.0;
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(Rect::from_min_size(pos2(0.0, 0.0), vec2(inner, HEIGHT)))
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                storage_footer(&mut child, &palette, &mut ed);
                measured = child.min_rect().height();
            });
        }
        assert!(
            measured <= FOOTER_RESERVE,
            "the footer draws {measured} points into a {FOOTER_RESERVE}-point reserve, \
             so it paints outside the rectangle the pane gives it",
        );
    }

    /// And every pane is the *height* it was given, which is what makes the
    /// rail beside it reach the bottom of the dialog.
    ///
    /// The rail is exactly the dialog's height by construction. The modal is as
    /// tall as the taller of the two, so a pane that overran left the rail —
    /// with the version and the licence at its foot — stopping short of the
    /// bottom edge. It overran by thirteen points, which is `FOOTER_RESERVE`
    /// having been estimated at 34 while the footer cost 47.
    #[test]
    fn the_rail_reaches_the_bottom_of_every_settings_pane() {
        let rail = measure(None).y;
        assert_eq!(rail, HEIGHT, "the rail is not the dialog's height");
        for tab in [
            SettingsTab::General,
            SettingsTab::InputAndPen,
            SettingsTab::Themes,
            SettingsTab::Shortcuts,
        ] {
            let height = measure(Some(tab)).y;
            assert!(
                height <= rail,
                "the {tab:?} pane is {height} points tall beside a {rail}-point rail, \
                 so the modal grows and the rail stops short",
            );
        }
    }

    /// A theme in a directory of the test's own, in hand, with the editor's
    /// fields filled from it.
    fn staged(tag: &str) -> (std::path::PathBuf, Editor, Themes) {
        let dir = std::env::temp_dir().join(format!("umber-themes-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut library = ThemeLibrary::load_from(&dir);
        let id = library
            .duplicate(
                "Mine",
                ThemeKind::Graphite,
                Palette::of(ThemeKind::Graphite),
            )
            .expect("a fresh directory");
        let mut state = Themes {
            store: Ok(std::sync::Arc::new(library)),
            filled_from: String::new(),
            hex: Vec::new(),
            name: String::new(),
            confirming: None,
        };
        let mut ed = Editor::default();
        ed.custom_theme = state.library().and_then(|l| l.get(&id).cloned());
        state.refill(&ed);
        (dir, ed, state)
    }

    /// The whole of what "make a theme, edit it, and have it survive a restart"
    /// comes down to, without a window: a colour typed into the editor reaches
    /// the palette the interface is drawn in **and** the file on disk, in the
    /// same gesture.
    ///
    /// There is no Save button, so this is the only thing standing between the
    /// page and the state it was in before — a theme editor that saved nothing.
    #[test]
    fn a_colour_typed_into_the_editor_reaches_the_file_it_came_from() {
        let (dir, mut ed, mut state) = staged("typed");
        let id = ed.custom_theme.as_ref().expect("in hand").id.clone();
        let at = Token::ALL
            .iter()
            .position(|t| *t == Token::Accent)
            .expect("the accent is a token");

        state.hex[at] = "#123456".to_owned();
        set_token(&mut ed, &mut state, Token::Accent, at);

        let wanted = Color32::from_rgb(0x12, 0x34, 0x56);
        assert_eq!(ed.palette().accent, wanted, "the interface did not follow");
        assert_eq!(state.hex[at], "#123456", "the field was not normalised");
        // Reopened from the directory, which is exactly what the next launch
        // does.
        let reopened = ThemeLibrary::load_from(&dir);
        assert_eq!(
            reopened.get(&id).expect("still there").palette.accent,
            wanted,
            "the edit did not reach the file, so a restart would lose it"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// New theme copies what is in front of you, puts it in hand, and writes
    /// it — and Delete takes it and its file away and falls back to the
    /// built-in it was made from.
    ///
    /// The two ends of the path the design drew and the code did not have: the
    /// card used to be dashed and dead with a tooltip saying so.
    #[test]
    fn new_theme_makes_one_that_is_in_hand_and_on_disk_and_delete_takes_it_back() {
        let dir = std::env::temp_dir().join(format!("umber-themes-new-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut state = Themes {
            store: Ok(std::sync::Arc::new(ThemeLibrary::load_from(&dir))),
            filled_from: String::new(),
            hex: Vec::new(),
            name: String::new(),
            confirming: None,
        };
        let mut ed = Editor::default();
        ed.ui.theme = ThemeKind::Paper;
        assert!(ed.custom_theme.is_none());

        new_theme(&mut state, &mut ed);
        let made = ed.custom_theme.clone().expect("New theme put one in hand");
        assert_eq!(made.base, ThemeKind::Paper, "it copied what was in front");
        assert_eq!(ed.palette(), Palette::of(ThemeKind::Paper));
        assert_ne!(made.name, "Paper", "and did not take the built-in's name");
        assert_eq!(
            ThemeLibrary::load_from(&dir)
                .get(&made.id)
                .map(|t| t.name.clone()),
            Some(made.name.clone()),
            "a theme that is only in memory is one a closed window loses"
        );
        assert_eq!(state.hex.len(), Token::ALL.len(), "the fields were filled");

        delete_theme(&mut state, &mut ed);
        assert!(ed.custom_theme.is_none());
        assert_eq!(
            ed.ui.theme,
            ThemeKind::Paper,
            "it must fall back to the built-in it was made from"
        );
        assert!(ThemeLibrary::load_from(&dir).themes().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A hex that will not read leaves the palette exactly as it was, and the
    /// field keeps what was typed so it can be corrected. A theme that quietly
    /// took black for a misread line would be a theme with an invisible
    /// interface in it.
    #[test]
    fn a_colour_that_will_not_read_changes_nothing() {
        let (dir, mut ed, mut state) = staged("bad");
        let before = ed.palette();
        let at = Token::ALL
            .iter()
            .position(|t| *t == Token::Window)
            .expect("a token");

        for bad in ["", "#12345", "rebeccapurple"] {
            state.hex[at] = bad.to_owned();
            assert!(
                !set_token(&mut ed, &mut state, Token::Window, at),
                "{bad} was read as a colour"
            );
            assert_eq!(ed.palette(), before, "{bad} moved the palette");
            // While the field has the caret this is right: it is what somebody
            // is typing. What puts the readout back is the *caller*, on the
            // blur — see `token_row`, and the test below.
            assert_eq!(state.hex[at], bad, "{bad} was taken out of the field");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Nothing the pane was part way through may outlive the page it was
    /// started on.
    ///
    /// Escape looks like it should abandon a field and does not: egui's
    /// `TextEdit` handles no `Key::Escape` and `egui::Modal` takes it to close
    /// the dialog, so the field is simply never drawn again and `lost_focus` is
    /// never seen. Left alone, reopening the page showed a name the theme did
    /// not have — whose next blur would apply the rename somebody thought they
    /// had cancelled — and a hex readout the chip beside it disagreed with.
    #[test]
    fn walking_away_from_the_page_forgets_what_was_half_typed() {
        let ctx = egui::Context::default();
        let (dir, ed, mut state) = staged("forget");
        let at = Token::ALL
            .iter()
            .position(|t| *t == Token::Window)
            .expect("a token");

        state.hex[at] = "rebeccapurple".to_owned();
        state.name = "half a name".to_owned();
        state.confirming = ed.custom_theme.as_ref().map(|t| t.id.clone());
        store_themes(&ctx, state);

        forget_themes_edit(&ctx);

        let mut back = ctx
            .data(|d| d.get_temp::<Themes>(themes_id()))
            .expect("the state is still there");
        assert!(back.confirming.is_none(), "a Delete stayed armed");
        assert!(
            back.library().is_some(),
            "the library was thrown away with the edit, so the next visit \
             re-reads the directory"
        );
        // And the way back in fills the fields from the theme, not from what
        // was abandoned.
        back.refill(&ed);
        assert_eq!(back.name, ed.custom_theme.as_ref().unwrap().name);
        assert_eq!(
            back.hex[at],
            themelib::hex(ed.palette().token(Token::Window))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The Themes page, in front of a built-in and in front of a theme somebody
    /// made.
    ///
    /// Written rather than asserted, for the reason `layers_panel_preview` is:
    /// the two things that went wrong here were a *layout* — a box wider than
    /// the pane, and two buttons drawn with no gap between them — and no
    /// assertion about widgets catches "these look like one control".
    /// `docshot::Stage` is the only thing in the crate that can look at a piece
    /// of interface.
    ///
    /// ```sh
    /// cargo test -p umber-app themes_pane_preview -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "writes preview PNGs and wants a GPU; run deliberately"]
    #[cfg(debug_assertions)]
    fn themes_pane_preview() {
        use crate::docshot;

        let Some(mut stage) = docshot::Stage::new() else {
            eprintln!("no GPU adapter: nothing to draw into. Skipped.");
            return;
        };
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/themes-pane");
        std::fs::create_dir_all(&dir).expect("create the preview directory");

        // A library of this test's own, so the picture does not depend on —
        // and cannot write into — whatever themes the machine running it has.
        let staged = dir.join("library");
        let _ = std::fs::remove_dir_all(&staged);
        let mut library = ThemeLibrary::load_from(&staged);
        let mut palette = Palette::of(ThemeKind::Graphite);
        palette.set_token(Token::Accent, egui::Color32::from_rgb(0x6E, 0x9E, 0xC8));
        let made = library
            .duplicate("Midnight oil", ThemeKind::Graphite, palette)
            .expect("a fresh directory");

        for (name, mine) in [
            ("1-built-in", None),
            (
                "2-custom",
                ThemeLibrary::load_from(&staged).get(&made).cloned(),
            ),
        ] {
            let mut ed = Editor::default();
            ed.layout = crate::dock::Layout::default();
            ed.ui.settings_open = true;
            ed.ui.settings_tab = SettingsTab::Themes;
            ed.custom_theme = mine;
            stage_themes(&stage.ctx, ThemeLibrary::load_from(&staged));
            let palette = ed.palette();
            let field = vec2(1048.0, 688.0);
            let image = stage.shoot(field, 1.5, &palette, palette.backdrop, |ui| {
                show(ui, &palette, &mut ed, &mut crate::ui::UiActions::default())
            });
            let written = docshot::write_png(&dir.join(format!("{name}.png")), &image)
                .expect("write the preview");
            println!("{}", written.0.display());
        }
    }
}
