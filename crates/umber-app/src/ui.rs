//! The Umber workspace.
//!
//! Layout follows the "Umber app" screen of the design project: menu bar, tool
//! options strip, a two-column tool rail, the canvas, and stacked modules
//! (Colour, Brushes, Layers) in a sidebar.
//!
//! Where those modules sit is no longer fixed. They can be dragged between the
//! two sidebars, reordered within one, torn off to float over the canvas, and
//! closed; the sidebars and the panels within them resize. That machinery lives
//! in [`crate::dock`] (the model) and [`crate::panels`] (the drawing) rather
//! than here, because this file was already long enough.
//!
//! There used to be a global "left-handed" flag that mirrored the whole
//! workspace. It is gone: a mirror is a worse version of "put the panels where
//! you want them", and the tool rail keeps a side of its own for the one thing
//! the mirror was actually for.

use crate::dock::Side;
use crate::editor::{BrushTab, Editor, Tool};
use crate::icons::{self, Icon};
use crate::panels;
use crate::shortcuts::{self, Action};
use crate::tabs;
use crate::theme::{Palette, metrics, text};
use crate::widgets;
use egui::{Align2, FontId, Frame, Margin, Rect, Sense, Stroke, pos2, vec2};
use umber_core::{Brush, ResponseCurve, input::PressureSource};

/// Requests the UI makes that need GPU access, handled by the caller.
#[derive(Default, Clone, Copy)]
pub struct UiActions {
    pub clear: bool,
    pub export: bool,
    pub undo: bool,
    pub redo: bool,
    pub fit_view: bool,
    pub reset_zoom: bool,
    pub add_layer: bool,
    pub delete_layer: Option<usize>,
    pub move_layer_up: Option<usize>,
    pub move_layer_down: Option<usize>,
    /// Make this document active. Every document has GPU storage of its own,
    /// so the switch is the caller's to carry out.
    pub pick_tab: Option<usize>,
    /// Close this document, having already been confirmed if it holds work.
    pub close_tab: Option<usize>,
    pub new_document: bool,
    pub open_file: bool,
}

pub struct UiOutput {
    pub actions: UiActions,
    /// Region left for the document, in egui points.
    pub canvas_rect: Rect,
}

/// egui 0.35 merged `SidePanel`/`TopBottomPanel` into one `Panel` type that
/// nests inside a `Ui` rather than attaching to the `Context`, which is why
/// this takes a `&mut Ui`.
pub fn draw(root: &mut egui::Ui, ed: &mut Editor) -> UiOutput {
    let p = Palette::with_accent(ed.ui.theme, ed.ui.accent);
    let mut actions = UiActions::default();

    // The design gives every chrome strip a hairline along the edge it meets
    // the next one at. egui's own panel separator is exactly that — it uses
    // `noninteractive.bg_stroke`, which `theme::apply` sets to the design's
    // border colour — so these are left with it switched on and draw none of
    // their own.
    let pad = Margin::symmetric(metrics::STRIP_PAD, 0);
    let chrome = Frame {
        fill: p.chrome,
        ..Default::default()
    };

    egui::Panel::top("menu-bar")
        .exact_size(metrics::MENU_BAR)
        .frame(chrome.inner_margin(pad))
        .show(root, |ui| menu_bar(ui, &p, ed, &mut actions));

    // Between the menu bar and the tool options, where the design draws it.
    // It takes its 30 points out of the window like any other panel, so the
    // canvas region — and with it the camera pivot every dab is placed against
    // — shrinks to match without anything here having to say so.
    let tab_strip = Frame {
        fill: p.dock,
        ..Default::default()
    };
    let mut tab_actions = tabs::TabActions::default();
    egui::Panel::top("doc-tabs")
        .exact_size(metrics::TAB_STRIP)
        .frame(tab_strip)
        // The one strip that draws its own rule, because the active tab has to
        // break through it to join the surface below. egui's separator goes on
        // top of the panel's own contents, so leaving it on drew a line straight
        // across the bottom of the selected tab.
        .show_separator_line(false)
        .show(root, |ui| tab_actions = tabs::strip(ui, &p, ed));

    egui::Panel::top("options-strip")
        .exact_size(metrics::OPTIONS_STRIP)
        .frame(chrome.inner_margin(pad))
        .show(root, |ui| options_strip(ui, &p, ed));

    egui::Panel::bottom("status-bar")
        .exact_size(metrics::STATUS_BAR)
        .frame(chrome.inner_margin(pad))
        .show(root, |ui| status_bar(ui, &p, ed, &mut actions));

    // Only present in layout edit mode, and claimed before the workspace is
    // measured so the sidebars sit under it rather than behind it.
    panels::edit_bar(root, &p, ed);

    // Everything below the strips and above the status bar is the layout's to
    // divide up. Measuring it here, before any of it is claimed, is what lets
    // the dock model compute every rect up front — so the drop indicator and
    // the panels it predicts cannot disagree.
    let workspace = root.available_rect_before_wrap();
    ed.layout.clamp_floating(workspace);
    let geo = ed.layout.geometry(workspace, metrics::TOOL_RAIL);

    let rail_frame = chrome.inner_margin(Margin::symmetric(0, 8));
    match ed.layout.rail_side() {
        Side::Left => egui::Panel::left("tool-rail"),
        Side::Right => egui::Panel::right("tool-rail"),
    }
    .exact_size(metrics::TOOL_RAIL)
    .frame(rail_frame)
    .show(root, |ui| tool_rail(ui, &p, ed));

    panels::sidebars(root, &p, ed, &mut actions, &geo);

    // The strip only reports; acting on it is the caller's, because every
    // document owns GPU storage that has to be created, switched or freed.
    actions.pick_tab = tab_actions.pick;
    actions.new_document |= tab_actions.new_document;
    if let Some(index) = tab_actions.close {
        if ed.session.tabs().get(index).is_some_and(|tab| tab.modified) {
            // Show the document before asking about it: a prompt that offers to
            // export a canvas you cannot see is asking about the wrong one.
            actions.pick_tab = Some(index);
            ed.ui.close_prompt = Some(index);
        } else {
            actions.close_tab = Some(index);
        }
    }

    brush_editor(root, &p, ed);
    crate::settings::show(root, &p, ed);

    match tabs::close_prompt(root, &p, ed) {
        Some(tabs::CloseChoice::Close) => actions.close_tab = ed.ui.close_prompt.take(),
        // Export is the one thing that can preserve the work, so the prompt
        // stays open behind it — exporting is not itself an answer to
        // "close this?".
        Some(tabs::CloseChoice::Export) => actions.export = true,
        Some(tabs::CloseChoice::Cancel) | None => {}
    }
    tabs::notice(root, &p, ed);

    // Whatever is left is the document's. The canvas is drawn by the GPU
    // beneath egui, so this panel only reports its rect and stays transparent.
    //
    // Floating panels are added *after* this deliberately. They are egui
    // `Area`s, which claim no space, so the canvas rect — and therefore
    // `Editor::canvas_pivot` and `CompositeParams::pivot` — is the same whether
    // a panel hovers over the canvas or not. Making them panels instead would
    // shrink this rect, move the pivot, and land every dab away from the
    // cursor.
    let canvas_rect = egui::CentralPanel::default()
        .frame(Frame::NONE)
        .show(root, |ui| ui.max_rect())
        .inner;

    panels::floats(root, &p, ed, &mut actions);
    panels::edit_mode_outline(root, &p, ed, &geo);
    // Last, so the drop it resolves is tested against a frame in which every
    // panel has already had its say.
    panels::drag_overlay(root, &p, ed, &geo);
    ed.layout.save_if_dirty();

    // Keys are read off the winit event before egui is asked, so a field with
    // the keyboard has to say so or every letter typed into it also fires a
    // tool shortcut. Asked here, once, for the whole interface: every text
    // field is drawn by the time this runs, and a per-module version only ever
    // covers the fields that module knows about — which is how the settings
    // dialog's search box came to be the one nobody had.
    //
    // `text_edit_focused` rather than `egui_wants_keyboard_input`: the latter
    // is true for anything focusable, so tabbing onto a button would leave the
    // canvas deaf to every shortcut until the focus was dropped again.
    shortcuts::set_typing(root.ctx().text_edit_focused());

    UiOutput {
        actions,
        canvas_rect,
    }
}

fn menu_bar(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor, actions: &mut UiActions) {
    ui.horizontal_centered(|ui| {
        let (mark, _) = ui.allocate_exact_size(vec2(15.0, 15.0), Sense::hover());
        ui.painter().rect_filled(mark, 3.0, p.accent);
        ui.add_space(6.0);

        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("New").clicked() {
                    actions.new_document = true;
                    ui.close();
                }
                if ui.button("Open…").clicked() {
                    actions.open_file = true;
                    ui.close();
                }
                // Only offered while there is another document to fall back to;
                // Umber has nowhere to go with nothing open.
                if ui
                    .add_enabled(ed.session.len() > 1, egui::Button::new("Close document"))
                    .on_disabled_hover_text(
                        "This is the only document open, and Umber has nothing to \
                         show in its place.",
                    )
                    .clicked()
                {
                    let index = ed.session.active_index();
                    if ed.session.active_tab().modified {
                        ed.ui.close_prompt = Some(index);
                    } else {
                        actions.close_tab = Some(index);
                    }
                    ui.close();
                }
                ui.separator();
                if ui.button("Clear layer").clicked() {
                    actions.clear = true;
                    ui.close();
                }
                if ui.button("Export flat PNG…").clicked() {
                    actions.export = true;
                    ui.close();
                }
                ui.separator();
                // Shown but inert: reading other applications' files works, but
                // Umber has no format of its own to write, and a Save that
                // cannot save would be worse than the gap it papers over.
                ui.add_enabled(false, egui::Button::new("Save"))
                    .on_disabled_hover_text(
                        "Umber has no document format yet — export a flat PNG instead",
                    );
            });

            ui.menu_button("Edit", |ui| {
                // The history covers painting only, which is why these can be
                // dead on a document that plainly has layers in it.
                if ui
                    .add_enabled(ed.history.can_undo(), egui::Button::new("Undo"))
                    .on_disabled_hover_text("Nothing painted on this document to undo.")
                    .clicked()
                {
                    actions.undo = true;
                    ui.close();
                }
                if ui
                    .add_enabled(ed.history.can_redo(), egui::Button::new("Redo"))
                    .on_disabled_hover_text("Nothing undone to put back.")
                    .clicked()
                {
                    actions.redo = true;
                    ui.close();
                }
            });

            ui.menu_button("View", |ui| {
                if ui.button("Fit to window").clicked() {
                    actions.fit_view = true;
                    ui.close();
                }
                if ui.button("Actual size").clicked() {
                    actions.reset_zoom = true;
                    ui.close();
                }
            });

            ui.menu_button("Window", |ui| {
                panels::window_menu(ui, ed);
                ui.separator();
                if ui.button("Settings…").clicked() {
                    ed.ui.settings_open = true;
                    ui.close();
                }
            });

            ui.menu_button("Help", |ui| {
                ui.label(
                    egui::RichText::new(format!("Umber {}", env!("CARGO_PKG_VERSION"))).strong(),
                );
                ui.label(egui::RichText::new("GPL-3.0-or-later").small().weak());
            });
        });

        // Where the menus finished, before the right-hand group moves the
        // cursor to the other end of the bar.
        let menus_right = ui.cursor().min.x;

        let right = ui
            .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icon_button(ui, p, Icon::Gear, true, "Settings") {
                    ed.ui.settings_open = true;
                }
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(format!("{:.0} fps", ed.average_fps()))
                        .size(text::TINY)
                        .color(p.text_dim),
                );
            })
            .response
            .rect;

        // Document title, centred on the band left over between the menus and
        // the frame counter rather than on the whole bar. Centring on the bar
        // reads better only while the bar is wide: below about 900 points the
        // title crossed the menu labels, and below 600 it sat under the gear.
        // Drawn after both, so the band is measured rather than guessed.
        let band = Rect::from_min_max(
            pos2(menus_right + 12.0, ui.max_rect().top()),
            pos2(right.left() - 12.0, ui.max_rect().bottom()),
        );
        if band.width() >= 40.0 {
            let title = format!(
                "{} — {} × {}",
                ed.session.active_title(),
                ed.doc.size.x,
                ed.doc.size.y
            );
            let painter = ui.painter();
            painter.text(
                band.center(),
                Align2::CENTER_CENTER,
                widgets::elide(painter, &title, text::CONTROL, band.width()),
                FontId::proportional(text::CONTROL),
                p.text_dim,
            );
        }
    });
}

/// What each optional group on the tool options strip costs, in points.
///
/// The strip is a single unwrapped row, so a window narrow enough to overrun it
/// does not reflow — the controls simply carry on past the right edge and under
/// the Edit brush link. These budgets decide which groups are drawn, in reverse
/// order of how constantly a painter reaches for them: the stabiliser readout
/// goes first, then opacity, then size.
///
/// They are the design's own widths (a 90 point rail, a 24 point readout) plus
/// the labels and egui's item spacing, rather than anything measured. Measuring
/// would mean laying the strip out twice to find out whether to lay it out, and
/// these only decide *whether* a group appears, never where it lands.
mod strip_budget {
    pub const SIZE: f32 = 160.0;
    pub const OPACITY: f32 = 185.0;
    pub const STABILISER: f32 = 110.0;
    /// Kept clear at the right for the link into the brush editor, which is the
    /// only way to reach half the brush's settings and so is never dropped.
    pub const EDIT_LINK: f32 = 92.0;
}

/// The horizontal strip of settings for the current tool.
///
/// Size and opacity live here as well as further down the dock because they are
/// the two a painter reaches for constantly.
fn options_strip(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    ui.horizontal_centered(|ui| {
        let (icon, name) = match ed.ui.tool {
            Tool::Brush => (Icon::Brush, "Brush"),
            Tool::Eraser => (Icon::Eraser, "Eraser"),
            Tool::Pan => (Icon::Pan, "Pan"),
            Tool::Zoom => (Icon::Zoom, "Zoom"),
        };
        let (glyph_rect, _) = ui.allocate_exact_size(vec2(15.0, 15.0), Sense::hover());
        icons::draw(ui.painter(), glyph_rect, icon, p.accent);
        ui.label(
            egui::RichText::new(name)
                .size(text::SMALL)
                .color(p.text_strong)
                .strong(),
        );

        divider(ui, p);

        if ed.ui.tool.paints() {
            let room = ui.available_width() - strip_budget::EDIT_LINK;
            if room >= strip_budget::SIZE {
                widgets::inline_slider(
                    ui,
                    p,
                    "Size",
                    &mut ed.brush.size,
                    Brush::MIN_SIZE..=400.0,
                    true,
                    |v| format!("{v:.0}"),
                );
            }
            if room >= strip_budget::SIZE + strip_budget::OPACITY {
                widgets::inline_slider(
                    ui,
                    p,
                    "Opacity",
                    &mut ed.brush.opacity,
                    0.0..=1.0,
                    false,
                    |v| format!("{:.0}", v * 100.0),
                );
            }
            if room >= strip_budget::SIZE + strip_budget::OPACITY + strip_budget::STABILISER {
                divider(ui, p);

                // Read-only, unlike the design's, which has a chevron and opens
                // a menu. Stabilisation is set in the brush editor; the tooltip
                // says so rather than leaving a pill that looks like a control
                // and answers to nothing.
                widgets::chip(
                    ui,
                    p,
                    "Stabiliser",
                    &format!("{:.0}", ed.brush.stabilization * 100.0),
                    "How much this brush smooths the stroke. Change it under \
                     Edit brush, on the Tip tab.",
                );
            }
        } else {
            ui.label(
                egui::RichText::new("drag on the canvas")
                    .size(text::SMALL)
                    .color(p.text_dim),
            );
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if text_icon_link(ui, p, Icon::Pencil, "Edit brush…")
                .on_hover_text("Open the brush editor")
                .clicked()
            {
                ed.ui.brush_editor_open = true;
            }
        });
    });
}

fn divider(ui: &mut egui::Ui, p: &Palette) {
    let (rect, _) = ui.allocate_exact_size(vec2(1.0, 16.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, p.border);
}

fn tool_rail(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    // The rail moves the same way everything else does — by being dragged in
    // layout edit mode. It deliberately has no side *setting*: a flag for
    // "which side is the chrome on" is the left-handed mirror under another
    // name, and that is the thing this branch removes.
    panels::rail_grip(ui, p, ed);

    ui.vertical_centered(|ui| {
        ui.spacing_mut().item_spacing = vec2(2.0, 2.0);

        // Two columns, as the design lays out its tool grid. Umber has four
        // tools where the design shows sixteen; the rest are simply not drawn,
        // rather than shown as buttons that do nothing.
        //
        // The keys come from the binding table rather than being written in:
        // these tooltips were a second copy of it, and rebinding the brush left
        // this one still promising `B`.
        let tools = [
            (
                Tool::Brush,
                Icon::Brush,
                shortcuts::labelled("Brush", Action::BrushTool),
            ),
            (
                Tool::Eraser,
                Icon::Eraser,
                shortcuts::labelled("Eraser", Action::EraserTool),
            ),
            (
                Tool::Pan,
                Icon::Pan,
                format!(
                    "{}, or hold Space",
                    shortcuts::labelled("Pan", Action::PanTool)
                ),
            ),
            (
                Tool::Zoom,
                Icon::Zoom,
                shortcuts::labelled("Zoom", Action::ZoomTool),
            ),
        ];
        let mut picked = None;
        for pair in tools.chunks(2) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (tool, icon, tip) in pair {
                    if widgets::tool_button(ui, p, *icon, ed.ui.tool == *tool, tip).clicked() {
                        picked = Some(*tool);
                    }
                }
            });
        }
        if let Some(tool) = picked {
            ed.set_tool(tool);
        }

        ui.add_space(6.0);
        let (line, _) = ui.allocate_exact_size(vec2(44.0, 1.0), Sense::hover());
        ui.painter().rect_filled(line, 0.0, p.border);
        ui.add_space(6.0);

        // Overlapping foreground/background wells, click to swap.
        let (well, response) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::click());
        let fg = Rect::from_min_size(well.left_top(), vec2(24.0, 24.0));
        let bg = Rect::from_min_size(well.left_top() + vec2(10.0, 10.0), vec2(24.0, 24.0));
        let to32 = |c: umber_core::Color| {
            let [r, g, b, _] = c.to_srgb_u8();
            egui::Color32::from_rgb(r, g, b)
        };
        let painter = ui.painter();
        for (rect, colour) in [(bg, ed.secondary), (fg, ed.color)] {
            painter.rect_filled(rect, metrics::RADIUS, to32(colour));
            painter.rect_stroke(
                rect,
                metrics::RADIUS,
                Stroke::new(1.0, p.popover_border),
                egui::StrokeKind::Outside,
            );
        }
        let swap = shortcuts::labelled("Swap colours", Action::SwapColours);
        if response.on_hover_text(&swap).clicked() {
            ed.swap_colors();
        }

        ui.add_space(4.0);
        // The design writes "X swap" under the wells. The key is the bound one,
        // for the same reason the tooltips above use it — and the caption goes
        // altogether when the command has no key, rather than naming none.
        if let Some(chord) = shortcuts::first_chord(Action::SwapColours) {
            ui.label(
                egui::RichText::new(format!("{chord} swap"))
                    .size(9.0)
                    .color(p.text_dim.gamma_multiply(0.8)),
            );
        }
    });
}

/// A bare 18×18 icon that acts as a button. Shared with `panels.rs`.
///
/// A disabled one still hovers, and still shows its tooltip — matching
/// [`crate::controls::icon_button`], and for the same reason. Several callers
/// pass the *reason* it is dead as the tooltip (the brush library's `＋` hands
/// over whatever went wrong with the library file), and while the hover was
/// skipped along with the click, none of those explanations ever reached the
/// screen: what was left was a greyed mark with nothing to say for itself.
pub fn icon_button(ui: &mut egui::Ui, p: &Palette, icon: Icon, enabled: bool, tip: &str) -> bool {
    let (rect, response) = ui.allocate_exact_size(
        vec2(18.0, 18.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let hovered = enabled && response.hovered();
    icons::draw(
        ui.painter(),
        rect,
        icon,
        if !enabled {
            p.text_dim.gamma_multiply(0.4)
        } else if hovered {
            p.text_strong
        } else {
            p.text_dim
        },
    );
    response.on_hover_text(tip).clicked()
}

/// An icon followed by a label, behaving as one clickable unit.
fn text_icon_link(ui: &mut egui::Ui, p: &Palette, icon: Icon, label: &str) -> egui::Response {
    let font = FontId::proportional(text::SMALL);
    let text_w = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), p.text_dim)
        .size()
        .x;
    let (rect, response) = ui.allocate_exact_size(vec2(text_w + 20.0, 18.0), Sense::click());
    let colour = if response.hovered() {
        p.text_strong
    } else {
        p.text_dim
    };
    icons::draw(
        ui.painter(),
        Rect::from_min_size(rect.left_top(), vec2(16.0, 18.0)),
        icon,
        colour,
    );
    ui.painter().text(
        rect.right_center(),
        Align2::RIGHT_CENTER,
        label,
        font,
        colour,
    );
    response
}

fn status_bar(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor, actions: &mut UiActions) {
    ui.horizontal_centered(|ui| {
        // The right-hand group goes on first, even though it reads last. It is
        // the half that must never be lost — the zoom, and the two links that
        // put the view back — whereas the left is a running commentary. Placing
        // it first is what lets the left side be measured against what is
        // actually left, instead of overrunning it on a narrow window.
        let right = ui
            .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{} × {} · {:.0}% · {} layer{} · undo {:.0} MB",
                        ed.doc.size.x,
                        ed.doc.size.y,
                        ed.camera.zoom * 100.0,
                        ed.layers.len(),
                        if ed.layers.len() == 1 { "" } else { "s" },
                        ed.history.used_bytes() as f32 / (1024.0 * 1024.0),
                    ))
                    .size(text::TINY)
                    .color(p.text_dim),
                );

                // Two words with no icon and no border, so what they do is
                // worth spelling out — with the key that also does it.
                ui.add_space(8.0);
                if status_link(
                    ui,
                    p,
                    "100%",
                    &shortcuts::labelled("Show the document at actual size", Action::ActualSize),
                ) {
                    actions.reset_zoom = true;
                }
                if status_link(
                    ui,
                    p,
                    "Fit",
                    &shortcuts::labelled("Fit the whole document in the window", Action::FitView),
                ) {
                    actions.fit_view = true;
                }
            })
            .response
            .rect;

        // The design swaps the whole status line while the layout is being
        // edited. Saying so here is what makes a paused canvas legible rather
        // than a bug.
        let (line, ink) = if ed.layout.edit_mode() {
            (
                "layout edit — nothing you draw changes; panels are the only \
                 thing that moves"
                    .to_string(),
                p.accent,
            )
        } else {
            // The document is named rather than saved: there is no file behind
            // it, so the line says what it is instead of pretending a path.
            let tab = ed.session.active_tab();
            (
                format!(
                    "{}{} · panels locked — Window, Customise layout",
                    tab.title,
                    if tab.modified { " · unsaved" } else { "" },
                ),
                p.text_dim,
            )
        };

        // Painted rather than laid out, so it can be cut to the room the right
        // half left. An `egui::Label` in a horizontal layout extends instead of
        // wrapping, and would have run straight under the zoom readout.
        let bar = ui.max_rect();
        let room = right.left() - 12.0 - bar.left();
        if room > 24.0 {
            let painter = ui.painter();
            painter.text(
                bar.left_center(),
                Align2::LEFT_CENTER,
                widgets::elide(painter, &line, text::TINY, room),
                FontId::proportional(text::TINY),
                ink,
            );
        }
    });
}

fn status_link(ui: &mut egui::Ui, p: &Palette, label: &str, tip: &str) -> bool {
    ui.add(
        egui::Label::new(
            egui::RichText::new(label)
                .size(text::TINY)
                .color(p.text_dim),
        )
        .sense(Sense::click()),
    )
    .on_hover_text(tip)
    .clicked()
}

fn toggle_row(ui: &mut egui::Ui, p: &Palette, label: &str, value: &mut bool) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(label)
                .size(text::SMALL)
                .color(p.text_dim),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            widgets::toggle(ui, p, value);
        });
    });
}

/// The brush editor, matching the design's dialog.
///
/// Holds every brush parameter that is not on the options strip, so the strip
/// can stay short. Edits apply live — there is no OK or Cancel, because a paint
/// app should let you see a change as you make it.
fn brush_editor(root: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    if !ed.ui.brush_editor_open {
        return;
    }

    let name = ed
        .active_preset
        .and_then(|i| ed.presets.get(i))
        .map(|preset| preset.name.clone())
        .unwrap_or_else(|| "Brush".to_string());

    let response = egui::Modal::new(egui::Id::new("brush-editor"))
        .frame(
            Frame::NONE
                .fill(p.popover)
                .stroke(Stroke::new(1.0, p.popover_border))
                .corner_radius(8)
                .inner_margin(Margin::same(18)),
        )
        .show(root.ctx(), |ui| {
            ui.set_width(430.0);

            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("Edit brush — {name}"))
                        .size(text::CONTROL)
                        .color(p.text_strong)
                        .strong(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon_button(ui, p, Icon::Close, true, "Close") {
                        ed.ui.brush_editor_open = false;
                    }
                });
            });

            ui.add_space(10.0);
            widgets::segmented(
                ui,
                p,
                &mut ed.ui.brush_tab,
                &[(BrushTab::Tip, "Tip"), (BrushTab::Dynamics, "Dynamics")],
            );
            ui.add_space(12.0);

            match ed.ui.brush_tab {
                BrushTab::Tip => brush_editor_tip(ui, p, ed),
                BrushTab::Dynamics => brush_editor_dynamics(ui, p, ed),
            }

            // The design's footer: name what you have made, or write it back
            // over the brush you started from.
            crate::brushlib::save_row(ui, p, ed);
        });

    // Clicking the backdrop or pressing Escape dismisses it.
    if response.should_close() {
        ed.ui.brush_editor_open = false;
    }
}

fn brush_editor_tip(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    ui.spacing_mut().item_spacing.y = 12.0;
    widgets::slider_row(
        ui,
        p,
        "Size",
        &mut ed.brush.size,
        Brush::MIN_SIZE..=400.0,
        true,
        |v| format!("{v:.0} px"),
    );
    widgets::slider_row(
        ui,
        p,
        "Hardness",
        &mut ed.brush.hardness,
        0.0..=1.0,
        false,
        |v| format!("{:.0}%", v * 100.0),
    );
    widgets::slider_row(
        ui,
        p,
        "Opacity",
        &mut ed.brush.opacity,
        0.0..=1.0,
        false,
        |v| format!("{:.0}%", v * 100.0),
    );
    widgets::slider_row(
        ui,
        p,
        "Spacing",
        &mut ed.brush.spacing,
        0.01..=0.5,
        true,
        |v| format!("{:.0}%", v * 100.0),
    );
    widgets::slider_row(
        ui,
        p,
        "Stabilisation",
        &mut ed.brush.stabilization,
        0.0..=0.95,
        false,
        |v| format!("{:.0}%", v * 100.0),
    );
}

fn brush_editor_dynamics(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    ui.spacing_mut().item_spacing.y = 10.0;

    ui.label(
        egui::RichText::new("Pressure source")
            .size(text::SMALL)
            .color(p.text_dim),
    );
    widgets::segmented(
        ui,
        p,
        &mut ed.pressure.source,
        &[
            (PressureSource::Device, "Device"),
            (PressureSource::Simulated, "Speed"),
            (PressureSource::Constant, "Off"),
        ],
    );
    if ed.pressure.source == PressureSource::Device {
        ui.label(
            egui::RichText::new(
                "Touch screens report real pressure. Desktop pens fall back to \
                 full pressure.",
            )
            .size(10.0)
            .color(p.text_dim),
        );
    }

    ui.add_space(4.0);

    ui.columns(2, |columns| {
        curve_column(
            &mut columns[0],
            p,
            "Pressure → size",
            "size",
            &mut ed.brush.pressure_size,
            &mut ed.brush.size_curve,
        );
        curve_column(
            &mut columns[1],
            p,
            "Pressure → opacity",
            "opacity",
            &mut ed.brush.pressure_opacity,
            &mut ed.brush.opacity_curve,
        );
    });

    ui.add_space(4.0);
    widgets::slider_row(
        ui,
        p,
        "Min size",
        &mut ed.brush.min_size_ratio,
        0.0..=1.0,
        false,
        |v| format!("{:.0}%", v * 100.0),
    );
}

/// One dynamics column: an on/off toggle, the curve, and its presets.
fn curve_column(
    ui: &mut egui::Ui,
    p: &Palette,
    label: &str,
    salt: &str,
    enabled: &mut bool,
    curve: &mut ResponseCurve,
) {
    toggle_row(ui, p, label, enabled);

    ui.add_space(6.0);

    // The curve stays visible when the mapping is off, but disabled, so its
    // shape is not a surprise the moment it is switched back on.
    ui.scope(|ui| {
        if !*enabled {
            ui.disable();
        }
        let size = ui.available_width().min(150.0);
        widgets::curve_editor(ui, p, curve, size);

        ui.add_space(6.0);
        let current = curve.preset_name().unwrap_or("Custom");
        egui::ComboBox::from_id_salt(("curve-preset", salt))
            .selected_text(egui::RichText::new(current).size(text::TINY).color(p.text))
            .width(size)
            .show_ui(ui, |ui| {
                for (name, preset) in ResponseCurve::PRESETS {
                    if ui
                        .selectable_label(curve.preset_name() == Some(name), name)
                        .clicked()
                    {
                        *curve = preset;
                    }
                }
            });
    });
}
