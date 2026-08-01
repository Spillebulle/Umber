//! The settings dialog, from the design's Settings screen.
//!
//! The design's shape: a modal with a left rail of six tabs — General, Input &
//! pen, Pressure, Themes, Shortcuts, Performance — and one pane at a time on
//! the right. Themes and Shortcuts are the two the design draws in full; the
//! rest it marks as outside the prototype.
//!
//! Here, a tab is live only if there is something behind it that works. General,
//! Pressure, Themes and Shortcuts are; Input & pen and Performance are shown
//! disabled, with a tooltip saying why, rather than opening onto a pane of
//! controls that do nothing.
//!
//! The page also owns the preferences file. [`show`] runs every frame whether
//! the dialog is open or not, so its first call is where stored settings are
//! read, and the frame after a change is where they are written.

use crate::autosave;
use crate::controls::{self, CapState, Captured, Glyph};
use crate::editor::Editor;
use crate::icons::{self, Icon};
use crate::prefs;
use crate::shortcuts::{self, Action, Binding};
use crate::theme::{Accent, Palette, ThemeKind, metrics, text};
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
    /// Designed but not built — nothing behind it works yet.
    InputAndPen,
    Pressure,
    Themes,
    Shortcuts,
    /// Designed but not built.
    Performance,
}

impl SettingsTab {
    /// The rail, in the design's order, with the reason a tab is dead.
    const RAIL: [(SettingsTab, &'static str, &'static str); 6] = [
        (SettingsTab::General, "General", ""),
        (
            SettingsTab::InputAndPen,
            "Input & pen",
            "Umber reads pointer events from the window system, which carries no \
             pen tilt or button mapping on desktop. There is nothing to configure \
             until a native tablet path exists.",
        ),
        (SettingsTab::Pressure, "Pressure", ""),
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

/// The design's dialog is 1000×640. It is clamped to the window, because a
/// modal wider than the screen has no way back out of its own corners.
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

/// Height the footer — the hairline, the path and the reset button — claims at
/// the bottom of every pane.
///
/// Named because two places have to agree on it: the pane, which pushes the
/// footer down by whatever is left, and the Shortcuts list, which grows to fill
/// that same space and would otherwise slide underneath it.
const FOOTER_RESERVE: f32 = 34.0;

/// Breathing space between a pane's last control and the footer's hairline.
const LIST_GAP: f32 = 12.0;

/// Gap between the theme cards.
const CARD_GAP: f32 = 12.0;

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

fn pane(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor, actions: &mut UiActions) {
    Frame::NONE
        .inner_margin(Margin::symmetric(28, 24))
        .show(ui, |ui| {
            ui.set_min_height(ui.available_height());
            ui.spacing_mut().item_spacing.y = 8.0;

            ui.horizontal(|ui| {
                let (title, blurb) = match ed.ui.settings_tab {
                    SettingsTab::General => (
                        "General",
                        "How the workspace itself behaves, before any document is open.",
                    ),
                    SettingsTab::Pressure => (
                        "Pressure",
                        "Where a stroke's pressure comes from, and how it responds.",
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
                    SettingsTab::InputAndPen | SettingsTab::Performance => ("", ""),
                };
                ui.vertical(|ui| {
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
            ui.add_space(10.0);

            match ed.ui.settings_tab {
                SettingsTab::General => general_pane(ui, p, ed, actions),
                SettingsTab::Pressure => pressure_pane(ui, p, ed),
                SettingsTab::Themes => themes_pane(ui, p, ed),
                SettingsTab::Shortcuts => shortcuts_pane(ui, p),
                // The rail cannot select these; a preferences file naming one
                // could, so they land somewhere rather than on a blank pane.
                SettingsTab::InputAndPen | SettingsTab::Performance => {
                    ed.ui.settings_tab = SettingsTab::General;
                }
            }

            let left = (ui.available_height() - FOOTER_RESERVE).max(0.0);
            ui.allocate_space(vec2(0.0, left));
            storage_footer(ui, p, ed);
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
        // The one deferred slider in the application. This one is drawn inside
        // the thing it scales, so applying it per frame moves the track out
        // from under the pointer and the knob runs away; see
        // `widgets::slider_row_deferred`.
        if widgets::slider_row_deferred(
            ui,
            p,
            "Interface scale",
            &mut scale,
            prefs::MIN_SCALE..=prefs::MAX_SCALE,
            false,
            |v| format!("{:.0}%", v * 100.0),
        ) {
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
    autosave_section(ui, p, ed, actions);
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
// Pressure
// ---------------------------------------------------------------------------

fn pressure_pane(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    controls::section(ui, p, "Source");

    let mut source = ed.pressure.source;
    ui.scope(|ui| {
        ui.set_max_width(320.0);
        if widgets::segmented(
            ui,
            p,
            &mut source,
            &[
                (PressureSource::Device, "Device"),
                (PressureSource::Simulated, "Speed"),
                (PressureSource::Constant, "Off"),
            ],
        ) {
            ed.pressure.source = source;
            prefs::mark_dirty();
        }
    });
    ui.add_space(6.0);

    match source {
        PressureSource::Device => controls::note(
            ui,
            p,
            "Touch screens report real pressure. Desktop pen tablets do not reach \
             Umber through the window system yet, so on a mouse or a desktop pen \
             this behaves as Off — pick Speed for a stand-in.",
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

// ---------------------------------------------------------------------------
// Themes
// ---------------------------------------------------------------------------

fn themes_pane(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    ui.horizontal(|ui| {
        // The dialog butts its rail against its pane with no gutter, and that
        // zero horizontal spacing is inherited all the way down here — which
        // left the theme cards touching. Set rather than left to the default,
        // because the default is whatever the enclosing layout last said.
        ui.spacing_mut().item_spacing.x = CARD_GAP;
        for kind in ThemeKind::ALL {
            if theme_card(ui, p, kind, ed.ui.theme == kind) {
                ed.ui.theme = kind;
                prefs::mark_dirty();
            }
        }
        new_theme_card(ui, p);
    });

    ui.add_space(14.0);
    theme_editor(ui, p, ed);

    ui.add_space(16.0);
    ui.label(
        egui::RichText::new("Layout")
            .size(text::SMALL)
            .color(p.text_dim),
    );
    ui.add_space(6.0);
    // The left-handed mirror that used to live here is gone. Every part of the
    // workspace, the tool rail included, now moves by being dragged in layout
    // edit mode, so a global handedness flag has nothing left to do.
    ui.label(
        egui::RichText::new(
            "Panels, sidebars and the tool rail are arranged by dragging them. \
             Turn on Window, Customise layout to move them; the same menu resets \
             the layout if it goes wrong.",
        )
        .size(10.0)
        .color(p.text_dim),
    );
}

/// A miniature of the workspace in that theme, so the choice is visual rather
/// than a name you have to try to remember the look of.
fn theme_card(ui: &mut egui::Ui, p: &Palette, kind: ThemeKind, selected: bool) -> bool {
    let swatch = Palette::of(kind);
    let (rect, response) = ui.allocate_exact_size(vec2(150.0, 104.0), Sense::click());

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
    painter.text(
        strip.left_center() + vec2(10.0, 0.0),
        Align2::LEFT_CENTER,
        kind.label(),
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

/// The design's dashed "New theme" card, drawn dead.
///
/// Themes are a compiled table of values, not a document — there is nothing to
/// create yet. Shown rather than dropped so the row matches the design and the
/// tooltip can say what is missing.
fn new_theme_card(ui: &mut egui::Ui, p: &Palette) {
    let (rect, response) = ui.allocate_exact_size(vec2(150.0, 104.0), Sense::hover());
    let painter = ui.painter();
    let dim = p.border;

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

    response.on_hover_text(
        "A theme is a table of values compiled into Umber, not a file. Making and \
         saving your own needs a theme format, which is not built.",
    );
}

/// The design's theme editor, as a read-only inspector.
///
/// Every row is a real token out of the palette in use, so it tells the truth
/// about the running theme; none of them can be edited, because a palette is
/// compiled in and there is nowhere to put a change.
fn theme_editor(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    Frame::NONE
        .fill(p.window)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(metrics::RADIUS_LARGE)
        .show(ui, |ui| {
            Frame::NONE
                .inner_margin(Margin::symmetric(14, 10))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Theme editor")
                                .size(text::CONTROL)
                                .color(p.text_strong)
                                .strong(),
                        );
                        ui.label(
                            egui::RichText::new("— read-only")
                                .size(10.0)
                                .color(p.text_dim),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let _ = controls::text_button(ui, p, "Save", false, false)
                                .on_hover_text(
                                    "Editing a palette needs somewhere to keep the result. \
                                     There is no theme format yet.",
                                );
                            let _ =
                                controls::text_button(ui, p, "Export .umbertheme", false, false)
                                    .on_hover_text("No theme format exists to export to.");
                        });
                    });
                });

            let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
            ui.painter().rect_filled(line, 0.0, p.border);

            Frame::NONE
                .inner_margin(Margin::symmetric(14, 10))
                .show(ui, |ui| {
                    let rows = [
                        ("Background", p.window),
                        ("Panel", p.chrome),
                        ("Canvas pit", p.backdrop),
                        ("Text", p.text),
                        ("Accent", p.accent),
                        ("Hairline", p.border),
                    ];
                    // Two columns, as the design lays them out.
                    let column = (ui.available_width() - 24.0) * 0.5;
                    for pair in rows.chunks(2) {
                        ui.horizontal(|ui| {
                            for (name, colour) in pair {
                                ui.scope(|ui| {
                                    ui.set_width(column);
                                    swatch_row(ui, p, name, *colour);
                                });
                                ui.add_space(24.0);
                            }
                        });
                    }

                    ui.add_space(8.0);
                    accent_choice(ui, p, ed);
                });
        });
}

/// One palette entry: a chip of the colour, its name, and its hex.
fn swatch_row(ui: &mut egui::Ui, p: &Palette, name: &str, colour: Color32) {
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
        ui.label(
            egui::RichText::new(name)
                .size(text::SMALL)
                .color(p.text_muted),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(format!(
                    "#{:02X}{:02X}{:02X}",
                    colour.r(),
                    colour.g(),
                    colour.b()
                ))
                .monospace()
                .size(text::TINY)
                .color(p.text),
            );
        });
    });
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
    // Fill the pane down to the footer rather than stopping at a fixed height.
    // This is the longest list in the dialog and the only one worth scrolling,
    // so a third of the pane sitting empty below it was wasted on the one tab
    // that could use it. The frame's own 1 px border is inside this, hence the
    // two; `LIST_GAP` is the breathing space above the footer's hairline.
    Frame::NONE
        .fill(p.window)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(metrics::RADIUS_LARGE)
        .show(ui, |ui| {
            let height = (ui.available_height() - FOOTER_RESERVE - LIST_GAP - 2.0).max(140.0);
            egui::ScrollArea::vertical()
                .max_height(height)
                .auto_shrink([false, false])
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
    let hovered = response.hovered();

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
    let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(line, 0.0, p.border);
    ui.add_space(8.0);

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
