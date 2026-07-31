//! The Graphite workspace.
//!
//! Layout follows screen 1b of the Umber design project: a menu bar, a 48 px
//! tool rail, the canvas, a 264 px tabbed panel, and a status bar. The whole
//! arrangement mirrors for left-handed use, which is why the rail and panel
//! sides are chosen rather than fixed.

use crate::editor::{Editor, PanelTab, Tool};
use crate::theme::{Palette, ThemeKind, metrics, text};
use crate::widgets::{self, ToolIcon};
use egui::{Align2, FontId, Frame, Margin, Rect, Sense, Stroke, vec2};
use umber_core::{BlendMode, Brush, Color, LayerStack, input::PressureSource};

/// Requests the UI makes that need GPU access, handled by the caller.
#[derive(Default, Clone, Copy)]
pub struct UiActions {
    pub clear: bool,
    pub undo: bool,
    pub redo: bool,
    pub fit_view: bool,
    pub reset_zoom: bool,
    pub add_layer: bool,
    pub delete_layer: Option<usize>,
    pub move_layer_up: Option<usize>,
    pub move_layer_down: Option<usize>,
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
    let p = Palette::of(ed.ui.theme);
    let mut actions = UiActions::default();

    let chrome = Frame {
        fill: p.chrome,
        ..Default::default()
    };

    egui::Panel::top("menu-bar")
        .exact_size(metrics::MENU_BAR)
        .frame(chrome.inner_margin(Margin::symmetric(12, 0)))
        .show(root, |ui| menu_bar(ui, &p, ed, &mut actions));

    egui::Panel::bottom("status-bar")
        .exact_size(metrics::STATUS_BAR)
        .frame(chrome.inner_margin(Margin::symmetric(12, 0)))
        .show(root, |ui| status_bar(ui, &p, ed, &mut actions));

    // Mirrored layout: the rail sits on the drawing-hand side so the hand does
    // not cover the panel it is reaching past.
    let (rail_id, panel_id) = ("tool-rail", "tool-panel");
    let rail_frame = chrome.inner_margin(Margin::symmetric(0, 8));
    let panel_frame = Frame {
        fill: p.chrome,
        ..Default::default()
    };

    if ed.ui.left_handed {
        egui::Panel::right(rail_id)
            .exact_size(metrics::TOOL_RAIL)
            .frame(rail_frame)
            .show(root, |ui| tool_rail(ui, &p, ed));
        egui::Panel::left(panel_id)
            .exact_size(metrics::PANEL)
            .frame(panel_frame)
            .show(root, |ui| tool_panel(ui, &p, ed, &mut actions));
    } else {
        egui::Panel::left(rail_id)
            .exact_size(metrics::TOOL_RAIL)
            .frame(rail_frame)
            .show(root, |ui| tool_rail(ui, &p, ed));
        egui::Panel::right(panel_id)
            .exact_size(metrics::PANEL)
            .frame(panel_frame)
            .show(root, |ui| tool_panel(ui, &p, ed, &mut actions));
    }

    // Whatever is left is the document's. The canvas is drawn by the GPU
    // beneath egui, so this panel only reports its rect and stays transparent.
    let canvas_rect = egui::CentralPanel::default()
        .frame(Frame::NONE)
        .show(root, |ui| ui.max_rect())
        .inner;

    UiOutput {
        actions,
        canvas_rect,
    }
}

fn menu_bar(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor, actions: &mut UiActions) {
    ui.horizontal_centered(|ui| {
        // Brand mark.
        let (mark, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
        ui.painter().rect_filled(mark, 3.0, p.accent);
        ui.add_space(6.0);

        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Clear layer").clicked() {
                    actions.clear = true;
                    ui.close();
                }
                ui.separator();
                // Shown but inert: saving is not built. A menu that lies about
                // what the app can do is worse than one that admits the gap.
                ui.add_enabled(false, egui::Button::new("Open…"))
                    .on_disabled_hover_text("Not implemented yet");
                ui.add_enabled(false, egui::Button::new("Save"))
                    .on_disabled_hover_text("Not implemented yet");
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
                ui.separator();
                for kind in ThemeKind::ALL {
                    if ui
                        .selectable_label(ed.ui.theme == kind, kind.label())
                        .clicked()
                    {
                        ed.ui.theme = kind;
                        ui.close();
                    }
                }
            });

            ui.menu_button("Window", |ui| {
                if ui
                    .checkbox(&mut ed.ui.left_handed, "Left-handed layout")
                    .clicked()
                {
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

        // Document title, centred across the whole bar rather than after the
        // menus, so it does not drift as menu labels change.
        let bar = ui.max_rect();
        ui.painter().text(
            bar.center(),
            Align2::CENTER_CENTER,
            format!("untitled — {} × {}", ed.doc.size.x, ed.doc.size.y),
            FontId::proportional(text::CONTROL),
            p.text_dim,
        );

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(format!("{:.0} fps", ed.average_fps()))
                    .small()
                    .color(p.text_dim),
            );
        });
    });
}

fn tool_rail(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    ui.vertical_centered(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;

        for (tool, icon, tip) in [
            (Tool::Brush, ToolIcon::Brush, "Brush (B)"),
            (Tool::Eraser, ToolIcon::Eraser, "Eraser (E)"),
            (Tool::Pan, ToolIcon::Pan, "Pan (H, or hold Space)"),
            (Tool::Zoom, ToolIcon::Zoom, "Zoom (Z)"),
        ] {
            if widgets::tool_button(ui, p, icon, ed.ui.tool == tool, tip).clicked() {
                ed.set_tool(tool);
            }
        }

        // Colour well pinned to the bottom of the rail.
        let remaining = ui.available_height() - 38.0;
        if remaining > 0.0 {
            ui.add_space(remaining);
        }

        let [r, g, b, _] = ed.color.to_srgb_u8();
        let mut rgba = egui::Color32::from_rgb(r, g, b);
        let (well, response) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::click());
        ui.painter().rect_filled(well, metrics::RADIUS_LARGE, rgba);
        ui.painter().rect_stroke(
            well,
            metrics::RADIUS_LARGE,
            Stroke::new(1.0, p.border),
            egui::StrokeKind::Inside,
        );
        if response.clicked() {
            ed.ui.picker_open = !ed.ui.picker_open;
        }

        // egui's own picker rather than the design's bespoke SV square — see
        // the README's note on what was and was not taken from the design.
        let popup = egui::Popup::from_response(&response)
            .open(ed.ui.picker_open)
            .show(|ui| {
                ui.spacing_mut().slider_width = 180.0;
                if egui::color_picker::color_picker_color32(
                    ui,
                    &mut rgba,
                    egui::color_picker::Alpha::Opaque,
                ) {
                    ed.color = Color::from_srgb_u8(rgba.r(), rgba.g(), rgba.b(), 255);
                }
            });
        if popup.is_none() {
            ed.ui.picker_open = false;
        }
    });
}

fn tool_panel(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor, actions: &mut UiActions) {
    widgets::tabs(
        ui,
        p,
        &mut ed.ui.tab,
        &[(PanelTab::Brush, "Brush"), (PanelTab::Layers, "Layers")],
    );

    // Action row is pinned to the bottom, so reserve it before the scroll area
    // claims the remaining height.
    let footer = 46.0;
    let body = (ui.available_height() - footer).max(0.0);

    egui::ScrollArea::vertical()
        .max_height(body)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            Frame::NONE
                .inner_margin(Margin::symmetric(metrics::PANEL_PAD as i8, 14))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 13.0;
                    match ed.ui.tab {
                        PanelTab::Brush => brush_tab(ui, p, ed),
                        PanelTab::Layers => layers_tab(ui, p, ed, actions),
                    }
                });
        });

    // Footer.
    ui.painter().line_segment(
        [
            ui.max_rect().left_bottom() - vec2(0.0, footer),
            ui.max_rect().right_bottom() - vec2(0.0, footer),
        ],
        Stroke::new(1.0, p.border),
    );
    Frame::NONE
        .inner_margin(Margin::symmetric(metrics::PANEL_PAD as i8, 10))
        .show(ui, |ui| {
            let gap = 6.0;
            let width = (ui.available_width() - gap * 2.0) / 3.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                if widgets::flat_button(ui, p, "Undo", width, ed.history.can_undo()).clicked() {
                    actions.undo = true;
                }
                if widgets::flat_button(ui, p, "Redo", width, ed.history.can_redo()).clicked() {
                    actions.redo = true;
                }
                if widgets::flat_button(ui, p, "Clear", width, true).clicked() {
                    actions.clear = true;
                }
            });
        });
}

fn brush_tab(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    let mut tool = ed.ui.tool;
    if widgets::pills(
        ui,
        p,
        &mut tool,
        &[(Tool::Brush, "Paint"), (Tool::Eraser, "Erase")],
    ) {
        ed.set_tool(tool);
    }

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

    separator(ui, p);

    widgets::section(ui, p, "Pressure", &mut ed.ui.pressure_open, Some("pro"));
    if ed.ui.pressure_open {
        Frame::NONE
            .inner_margin(Margin {
                left: 16,
                ..Default::default()
            })
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 9.0;

                toggle_row(ui, p, "Pressure → size", &mut ed.brush.pressure_size);
                toggle_row(ui, p, "Pressure → opacity", &mut ed.brush.pressure_opacity);

                widgets::slider_row(
                    ui,
                    p,
                    "Min size",
                    &mut ed.brush.min_size_ratio,
                    0.0..=1.0,
                    false,
                    |v| format!("{:.0}%", v * 100.0),
                );

                ui.label(
                    egui::RichText::new("Source")
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
                            "Touch screens report real pressure. Desktop pens \
                             fall back to full pressure.",
                        )
                        .size(10.0)
                        .color(p.text_dim),
                    );
                }
            });
    }
}

fn layers_tab(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor, actions: &mut UiActions) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Layers")
                .size(text::CONTROL)
                .color(p.text)
                .strong(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let room = ed.layers.len() < LayerStack::MAX;
            if ui
                .add_enabled(room, egui::Button::new("+").small())
                .on_hover_text("Add a layer above the current one")
                .clicked()
            {
                actions.add_layer = true;
            }
        });
    });

    // Stored bottom-first; shown top-first, the way it is drawn.
    let active = ed.layers.active_index();
    let count = ed.layers.len();
    let mut select = None;

    egui::ScrollArea::vertical()
        .id_salt("layer-list")
        .max_height(200.0)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            for index in (0..count).rev() {
                let Some(layer) = ed.layers.get_mut(index) else {
                    continue;
                };
                let is_active = index == active;

                let (row, response) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
                let painter = ui.painter();
                if is_active {
                    painter.rect_filled(row, metrics::RADIUS, p.control_active);
                } else if response.hovered() {
                    painter.rect_filled(row, metrics::RADIUS, p.control);
                }

                // The eye is its own hit target inside the row, so toggling
                // visibility does not also change the selection.
                let eye = Rect::from_min_size(row.left_top() + vec2(4.0, 4.0), vec2(18.0, 18.0));
                let eye_response = ui.interact(eye, ui.id().with(("eye", index)), Sense::click());
                if eye_response.clicked() {
                    layer.visible = !layer.visible;
                }
                let painter = ui.painter();
                painter.text(
                    eye.center(),
                    Align2::CENTER_CENTER,
                    if layer.visible { "◉" } else { "○" },
                    FontId::proportional(text::SMALL),
                    if layer.visible { p.text } else { p.text_dim },
                );
                painter.text(
                    row.left_center() + vec2(28.0, 0.0),
                    Align2::LEFT_CENTER,
                    &layer.name,
                    FontId::proportional(text::CONTROL),
                    match (is_active, layer.visible) {
                        (true, _) => p.text_strong,
                        (false, true) => p.text,
                        (false, false) => p.text_dim,
                    },
                );

                if response.clicked() && !eye_response.clicked() {
                    select = Some(index);
                }
            }
        });

    if let Some(index) = select {
        ed.layers.set_active(index);
    }

    ui.horizontal(|ui| {
        let gap = 6.0;
        let width = (ui.available_width() - gap * 2.0) / 3.0;
        ui.spacing_mut().item_spacing.x = gap;
        if widgets::flat_button(ui, p, "↑", width, active + 1 < count).clicked() {
            actions.move_layer_up = Some(active);
        }
        if widgets::flat_button(ui, p, "↓", width, active > 0).clicked() {
            actions.move_layer_down = Some(active);
        }
        // The last layer stays: a document needs somewhere to paint.
        if widgets::flat_button(ui, p, "Delete", width, count > 1)
            .on_hover_text("Deleting a layer clears undo history")
            .clicked()
        {
            actions.delete_layer = Some(active);
        }
    });

    separator(ui, p);

    let layer = ed.layers.active_mut();
    widgets::slider_row(
        ui,
        p,
        "Layer opacity",
        &mut layer.opacity,
        0.0..=1.0,
        false,
        |v| format!("{:.0}%", v * 100.0),
    );

    ui.label(
        egui::RichText::new("Blend")
            .size(text::SMALL)
            .color(p.text_dim),
    );
    egui::ComboBox::from_id_salt("blend-mode")
        .selected_text(layer.blend.label())
        .width(ui.available_width())
        .show_ui(ui, |ui| {
            for mode in BlendMode::ALL {
                ui.selectable_value(&mut layer.blend, mode, mode.label());
            }
        });
}

fn status_bar(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor, actions: &mut UiActions) {
    ui.horizontal_centered(|ui| {
        ui.label(
            egui::RichText::new("untitled.umber")
                .size(text::TINY)
                .color(p.text_dim),
        );

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let hand = if ed.ui.left_handed {
                "⇋ left"
            } else {
                "⇋ right"
            };
            if ui
                .add(
                    egui::Label::new(egui::RichText::new(hand).size(text::TINY).color(p.accent))
                        .sense(Sense::click()),
                )
                .on_hover_text("Mirror the layout for left-handed use")
                .clicked()
            {
                ed.ui.left_handed = !ed.ui.left_handed;
            }

            ui.add_space(8.0);
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

fn separator(ui: &mut egui::Ui, p: &Palette) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().rect_filled(rect, 0.0, p.border);
}
