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
use crate::tabs;
use crate::theme::{Palette, metrics, text};
use crate::widgets;
use egui::{Align2, FontId, Frame, Margin, Rect, Sense, Stroke, vec2};
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

    let chrome = Frame {
        fill: p.chrome,
        ..Default::default()
    };

    egui::Panel::top("menu-bar")
        .exact_size(metrics::MENU_BAR)
        .frame(chrome.inner_margin(Margin::symmetric(12, 0)))
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
        .exact_size(tabs::STRIP)
        .frame(tab_strip)
        .show(root, |ui| tab_actions = tabs::strip(ui, &p, ed));

    egui::Panel::top("options-strip")
        .exact_size(metrics::OPTIONS_STRIP)
        .frame(chrome.inner_margin(Margin::symmetric(12, 0)))
        .show(root, |ui| options_strip(ui, &p, ed));

    egui::Panel::bottom("status-bar")
        .exact_size(metrics::STATUS_BAR)
        .frame(chrome.inner_margin(Margin::symmetric(12, 0)))
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
                if ui
                    .add_enabled(ed.history.can_undo(), egui::Button::new("Undo"))
                    .clicked()
                {
                    actions.undo = true;
                    ui.close();
                }
                if ui
                    .add_enabled(ed.history.can_redo(), egui::Button::new("Redo"))
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

        // Document title, centred on the whole bar rather than after the menus,
        // so it does not drift as menu labels change.
        let bar = ui.max_rect();
        ui.painter().text(
            bar.center(),
            Align2::CENTER_CENTER,
            format!(
                "{} — {} × {}",
                ed.session.active_title(),
                ed.doc.size.x,
                ed.doc.size.y
            ),
            FontId::proportional(text::CONTROL),
            p.text_dim,
        );

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if icon_button(ui, p, Icon::Gear, true, "Settings") {
                ed.ui.settings_open = true;
            }
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(format!("{:.0} fps", ed.average_fps()))
                    .size(text::TINY)
                    .color(p.text_dim),
            );
        });
    });
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
            widgets::inline_slider(
                ui,
                p,
                "Size",
                &mut ed.brush.size,
                Brush::MIN_SIZE..=400.0,
                true,
                |v| format!("{v:.0}"),
            );
            widgets::inline_slider(
                ui,
                p,
                "Opacity",
                &mut ed.brush.opacity,
                0.0..=1.0,
                false,
                |v| format!("{:.0}", v * 100.0),
            );

            divider(ui, p);

            widgets::chip(
                ui,
                p,
                "Stabiliser",
                &format!("{:.0}", ed.brush.stabilization * 100.0),
            );
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
        let tools = [
            (Tool::Brush, Icon::Brush, "Brush (B)"),
            (Tool::Eraser, Icon::Eraser, "Eraser (E)"),
            (Tool::Pan, Icon::Pan, "Pan (H, or hold Space)"),
            (Tool::Zoom, Icon::Zoom, "Zoom (Z)"),
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
        if response.on_hover_text("Swap colours (X)").clicked() {
            ed.swap_colors();
        }

        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("X swap")
                .size(9.0)
                .color(p.text_dim.gamma_multiply(0.8)),
        );
    });
}

/// A bare 18×18 icon that acts as a button. Shared with `panels.rs`.
pub fn icon_button(ui: &mut egui::Ui, p: &Palette, icon: Icon, enabled: bool, tip: &str) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
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
    enabled && response.on_hover_text(tip).clicked()
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
        // The design swaps the whole status line while the layout is being
        // edited. Saying so here is what makes a paused canvas legible rather
        // than a bug.
        if ed.layout.edit_mode() {
            ui.label(
                egui::RichText::new(
                    "layout edit — nothing you draw changes; panels are the only \
                     thing that moves",
                )
                .size(text::TINY)
                .color(p.accent),
            );
        } else {
            // The document is named rather than saved: there is no file behind
            // it, so the line says what it is instead of pretending a path.
            let tab = ed.session.active_tab();
            ui.label(
                egui::RichText::new(format!(
                    "{}{} · panels locked — Window, Customise layout",
                    tab.title,
                    if tab.modified { " · unsaved" } else { "" },
                ))
                .size(text::TINY)
                .color(p.text_dim),
            );
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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

            ui.add_space(8.0);
            if status_link(ui, p, "100%") {
                actions.reset_zoom = true;
            }
            if status_link(ui, p, "Fit") {
                actions.fit_view = true;
            }
        });
    });
}

fn status_link(ui: &mut egui::Ui, p: &Palette, label: &str) -> bool {
    ui.add(
        egui::Label::new(
            egui::RichText::new(label)
                .size(text::TINY)
                .color(p.text_dim),
        )
        .sense(Sense::click()),
    )
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
            // Wider than the other modals because the Tip section is the
            // design's two-column grid and the Dynamics section is three curves
            // side by side. At 430 px either would have to stack, and a brush
            // editor you have to scroll is one you stop reaching for.
            ui.set_width(metrics::BRUSH_EDITOR_WIDTH);

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
                &[
                    (BrushTab::Tip, "Tip"),
                    (BrushTab::Dynamics, "Dynamics"),
                    (BrushTab::Scatter, "Scatter"),
                    (BrushTab::Blending, "Blending"),
                ],
            );
            ui.add_space(12.0);

            match ed.ui.brush_tab {
                BrushTab::Tip => brush_editor_tip(ui, p, ed),
                BrushTab::Dynamics => brush_editor_dynamics(ui, p, ed),
                BrushTab::Scatter => brush_editor_scatter(ui, p, ed),
                BrushTab::Blending => brush_editor_blending(ui, p, ed),
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

/// A percentage readout, which most of these sliders share.
fn percent(v: f32) -> String {
    format!("{:.0}%", v * 100.0)
}

/// A caption under a control, explaining why it is off or what it does.
fn caption(ui: &mut egui::Ui, p: &Palette, line: &str) {
    ui.label(egui::RichText::new(line).size(10.0).color(p.text_dim));
}

/// The design's Tip section: a two-column grid of the dab's own properties.
fn brush_editor_tip(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    ui.spacing_mut().item_spacing.y = 12.0;

    ui.columns(2, |c| {
        widgets::slider_row(
            &mut c[0],
            p,
            "Size",
            &mut ed.brush.size,
            Brush::MIN_SIZE..=400.0,
            true,
            |v| format!("{v:.0} px"),
        );
        widgets::slider_row(
            &mut c[1],
            p,
            "Hardness",
            &mut ed.brush.hardness,
            0.0..=1.0,
            false,
            percent,
        );
    });
    ui.columns(2, |c| {
        widgets::slider_row(
            &mut c[0],
            p,
            "Opacity",
            &mut ed.brush.opacity,
            0.0..=1.0,
            false,
            percent,
        );
        widgets::slider_row(
            &mut c[1],
            p,
            "Spacing",
            &mut ed.brush.spacing,
            0.01..=0.5,
            true,
            percent,
        );
    });
    ui.columns(2, |c| {
        // Roundness rather than the engine's aspect ratio, because that is the
        // word the design uses and the word every other paint application uses.
        // `dab_ratio` is long-over-short, so the two are reciprocals; 5% is the
        // floor because a 20:1 chisel is already thinner than any real bristle.
        let mut roundness = 1.0 / ed.brush.dab_ratio.max(1.0);
        if widgets::slider_row(
            &mut c[0],
            p,
            "Roundness",
            &mut roundness,
            0.05..=1.0,
            false,
            percent,
        ) {
            ed.brush.dab_ratio = 1.0 / roundness.clamp(0.05, 1.0);
        }
        widgets::slider_row(
            &mut c[1],
            p,
            "Airbrush rate",
            &mut ed.brush.dabs_per_second,
            0.0..=100.0,
            false,
            |v| {
                if v <= 0.0 {
                    "off".to_string()
                } else {
                    format!("{v:.0}/s")
                }
            },
        );
    });
    ui.columns(2, |c| {
        // A circle has no angle. Rather than let the slider lie, it is disabled
        // until the dab is elliptical and says why.
        let round = !ed.brush.dab_has_angle();
        c[0].scope(|ui| {
            if round {
                ui.disable();
            }
            widgets::slider_row(
                ui,
                p,
                "Angle",
                &mut ed.brush.dab_angle,
                0.0..=359.0,
                false,
                |v| format!("{v:.0}°"),
            );
        });
        widgets::slider_row(
            &mut c[1],
            p,
            "Stabilisation",
            &mut ed.brush.stabilization,
            0.0..=0.95,
            false,
            percent,
        );
    });

    ui.scope(|ui| {
        if !ed.brush.dab_has_angle() {
            ui.disable();
        }
        toggle_row(
            ui,
            p,
            "Angle follows the stroke",
            &mut ed.brush.dab_angle_follows_stroke,
        );
    });
    caption(
        ui,
        p,
        if ed.brush.dab_has_angle() {
            "A rake keeps its bristles across the line of travel; a broad nib \
             holds one angle through a curve."
        } else {
            "Angle needs an elliptical dab — lower Roundness first."
        },
    );

    ui.add_space(2.0);
    caption(
        ui,
        p,
        "Airbrush rate keeps depositing paint while the pen is held still. \
         Spacing alone stops when you do.",
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
        caption(
            ui,
            p,
            "Touch screens report real pressure. Desktop pens fall back to \
             full pressure.",
        );
    }

    ui.add_space(4.0);

    // Three curves rather than the design's two. Hardness is the most used
    // pressure dynamic in the shipped library after size and opacity — 69 of
    // its 196 brushes ask for it — and a light stroke that thins without also
    // softening does not read as a pencil.
    ui.columns(3, |c| {
        curve_column(
            &mut c[0],
            p,
            "Pressure → size",
            "size",
            &mut ed.brush.pressure_size,
            &mut ed.brush.size_curve,
            Some(("Min size", &mut ed.brush.min_size_ratio)),
        );
        curve_column(
            &mut c[1],
            p,
            "Pressure → opacity",
            "opacity",
            &mut ed.brush.pressure_opacity,
            &mut ed.brush.opacity_curve,
            None,
        );
        curve_column(
            &mut c[2],
            p,
            "Pressure → hardness",
            "hardness",
            &mut ed.brush.pressure_hardness,
            &mut ed.brush.hardness_curve,
            Some(("Min hardness", &mut ed.brush.min_hardness_ratio)),
        );
    });
}

/// The design's Scatter section: everything the dab does at random.
fn brush_editor_scatter(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    ui.spacing_mut().item_spacing.y = 12.0;

    ui.columns(2, |c| {
        // Stated in dab radii, so a brush sprays the same way at any size.
        widgets::slider_row(
            &mut c[0],
            p,
            "Scatter",
            &mut ed.brush.scatter,
            0.0..=8.0,
            false,
            |v| format!("{v:.2}×"),
        );
        widgets::slider_row(
            &mut c[1],
            p,
            "Size jitter",
            &mut ed.brush.radius_jitter,
            0.0..=2.0,
            false,
            |v| format!("{v:.2}"),
        );
    });

    ui.columns(2, |c| {
        let round = !ed.brush.dab_has_angle();
        c[0].scope(|ui| {
            if round {
                ui.disable();
            }
            widgets::slider_row(
                ui,
                p,
                "Angle jitter",
                &mut ed.brush.dab_angle_jitter,
                0.0..=360.0,
                false,
                |v| format!("±{:.0}°", v * 0.5),
            );
        });
        // A curve rather than a fourth column on Dynamics: pressure-driven
        // scatter is a property of the scatter, and it is unreadable next to
        // three curves that are all about the mark rather than its randomness.
        curve_column(
            &mut c[1],
            p,
            "Pressure → scatter",
            "scatter",
            &mut ed.brush.pressure_scatter,
            &mut ed.brush.scatter_curve,
            Some(("Min scatter", &mut ed.brush.min_scatter_ratio)),
        );
    });

    caption(
        ui,
        p,
        "Scatter is measured in dab radii, so a spray looks like itself at any \
         size. Angle jitter needs an elliptical dab to show.",
    );
}

/// Colour pickup — a brush that carries what it finds on the canvas.
fn brush_editor_blending(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    ui.spacing_mut().item_spacing.y = 12.0;

    widgets::slider_row(
        ui,
        p,
        "Colour pickup",
        &mut ed.brush.smudge,
        0.0..=1.0,
        false,
        percent,
    );

    // The other two only mean anything once something is being picked up, and
    // `smudges()` is the same threshold the renderer uses to decide whether to
    // run a canvas probe at all — so a control that is live here is a control
    // whose effect is actually rendered.
    let blending = ed.brush.smudges();
    ui.scope(|ui| {
        if !blending {
            ui.disable();
        }
        ui.spacing_mut().item_spacing.y = 12.0;
        ui.columns(2, |c| {
            widgets::slider_row(
                &mut c[0],
                p,
                "Smear length",
                &mut ed.brush.smudge_length,
                0.0..=0.99,
                false,
                percent,
            );
            widgets::slider_row(
                &mut c[1],
                p,
                "Pickup radius",
                &mut ed.brush.smudge_radius,
                0.25..=8.0,
                true,
                |v| format!("{v:.2}×"),
            );
        });
    });

    caption(
        ui,
        p,
        if blending {
            "Colour pickup mixes what is under the brush into what it deposits; \
             at 100% it deposits only what it found. Smear length is how long \
             that colour survives, pickup radius how wide a patch it averages."
        } else {
            "Raise colour pickup to turn this into a blender. The canvas is \
             sampled once a frame while a stroke is live, so it costs nothing \
             until you do."
        },
    );
}

/// One dynamics column: an on/off toggle, the curve, its presets, and — where
/// the parameter has a floor rather than falling to zero — that floor.
fn curve_column(
    ui: &mut egui::Ui,
    p: &Palette,
    label: &str,
    salt: &str,
    enabled: &mut bool,
    curve: &mut ResponseCurve,
    min: Option<(&str, &mut f32)>,
) {
    toggle_row(ui, p, label, enabled);

    ui.add_space(6.0);

    // The curve stays visible when the mapping is off, but disabled, so its
    // shape is not a surprise the moment it is switched back on.
    ui.scope(|ui| {
        if !*enabled {
            ui.disable();
        }
        let size = ui.available_width().min(metrics::CURVE_PANEL);
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

        if let Some((label, value)) = min {
            ui.add_space(8.0);
            widgets::slider_row(ui, p, label, value, 0.0..=1.0, false, percent);
        }
    });
}
