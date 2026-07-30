//! The tool panel.

use crate::editor::Editor;
use umber_core::{BlendMode, Brush, BrushMode, Color, LayerStack, input::PressureSource};

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

/// egui 0.35 merged `SidePanel`/`TopBottomPanel` into one `Panel` type that
/// nests inside a `Ui` rather than attaching to the `Context`, which is why
/// this takes a `&mut Ui`.
pub fn draw(root: &mut egui::Ui, ed: &mut Editor) -> UiActions {
    let mut actions = UiActions::default();

    egui::Panel::left("tools")
        .resizable(false)
        .exact_size(248.0)
        .show(root, |ui| {
            ui.add_space(8.0);
            ui.heading("Umber");
            ui.label(
                egui::RichText::new(format!("{:.0} fps", ed.average_fps()))
                    .small()
                    .weak(),
            );
            ui.separator();

            ui.horizontal(|ui| {
                let painting = ed.brush.mode == BrushMode::Paint;
                if ui.selectable_label(painting, "Brush").clicked() {
                    ed.brush.mode = BrushMode::Paint;
                }
                if ui.selectable_label(!painting, "Eraser").clicked() {
                    ed.brush.mode = BrushMode::Erase;
                }
            });

            ui.add_space(8.0);

            if ed.brush.mode == BrushMode::Paint {
                ui.horizontal(|ui| {
                    ui.label("Colour");
                    let [r, g, b, a] = ed.color.to_srgb_u8();
                    let mut c = egui::Color32::from_rgba_unmultiplied(r, g, b, a);
                    if egui::color_picker::color_edit_button_srgba(
                        ui,
                        &mut c,
                        egui::color_picker::Alpha::Opaque,
                    )
                    .changed()
                    {
                        ed.color = Color::from_srgb_u8(c.r(), c.g(), c.b(), 255);
                    }
                });
                ui.add_space(8.0);
            }

            ui.add(
                egui::Slider::new(&mut ed.brush.size, Brush::MIN_SIZE..=400.0)
                    .logarithmic(true)
                    .text("Size"),
            );
            ui.add(egui::Slider::new(&mut ed.brush.hardness, 0.0..=1.0).text("Hardness"));
            ui.add(egui::Slider::new(&mut ed.brush.opacity, 0.0..=1.0).text("Opacity"));
            ui.add(
                egui::Slider::new(&mut ed.brush.spacing, 0.01..=0.5)
                    .logarithmic(true)
                    .text("Spacing"),
            );
            ui.add(
                egui::Slider::new(&mut ed.brush.stabilization, 0.0..=0.95).text("Stabilisation"),
            );

            ui.add_space(10.0);
            ui.collapsing("Pressure", |ui| {
                ui.checkbox(&mut ed.brush.pressure_size, "Pressure → size");
                ui.checkbox(&mut ed.brush.pressure_opacity, "Pressure → opacity");
                ui.add(egui::Slider::new(&mut ed.brush.min_size_ratio, 0.0..=1.0).text("Min size"));

                ui.add_space(6.0);
                ui.label("Source");
                let src = &mut ed.pressure.source;
                ui.radio_value(src, PressureSource::Device, "Device");
                ui.radio_value(src, PressureSource::Simulated, "From speed");
                ui.radio_value(src, PressureSource::Constant, "Off");

                if *src == PressureSource::Device {
                    ui.label(
                        egui::RichText::new(
                            "Touch screens report real pressure. Desktop pens \
                             fall back to full pressure — see README.",
                        )
                        .small()
                        .weak(),
                    );
                }
            });

            ui.add_space(10.0);
            ui.separator();

            ui.horizontal(|ui| {
                if ui
                    .add_enabled(ed.history.can_undo(), egui::Button::new("Undo"))
                    .clicked()
                {
                    actions.undo = true;
                }
                if ui
                    .add_enabled(ed.history.can_redo(), egui::Button::new("Redo"))
                    .clicked()
                {
                    actions.redo = true;
                }
            });

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Fit").clicked() {
                    actions.fit_view = true;
                }
                if ui.button("100%").clicked() {
                    actions.reset_zoom = true;
                }
            });

            ui.add_space(6.0);
            if ui.button("Clear layer").clicked() {
                actions.clear = true;
            }

            ui.add_space(12.0);
            ui.separator();
            layer_panel(ui, ed, &mut actions);

            ui.add_space(10.0);
            ui.label(
                egui::RichText::new(format!(
                    "{}×{} · {:.0}% · undo {:.0} MB",
                    ed.doc.size.x,
                    ed.doc.size.y,
                    ed.camera.zoom * 100.0,
                    ed.history.used_bytes() as f32 / (1024.0 * 1024.0),
                ))
                .small()
                .weak(),
            );

            ui.add_space(10.0);
            ui.collapsing("Shortcuts", |ui| {
                for (key, what) in [
                    ("B / E", "brush / eraser"),
                    ("[ / ]", "size down / up"),
                    ("Ctrl+Z", "undo"),
                    ("Ctrl+Shift+Z", "redo"),
                    ("Space+drag", "pan"),
                    ("Middle drag", "pan"),
                    ("Wheel", "zoom"),
                    ("Ctrl+0", "fit"),
                ] {
                    ui.label(
                        egui::RichText::new(format!("{key:<14}{what}"))
                            .small()
                            .monospace(),
                    );
                }
            });
        });

    actions
}

/// The layer stack, listed top-down the way it is drawn.
fn layer_panel(ui: &mut egui::Ui, ed: &mut Editor, actions: &mut UiActions) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Layers").strong());
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

    ui.add_space(4.0);

    // The stack is stored bottom-first; show it top-first, as it appears.
    let active = ed.layers.active_index();
    let count = ed.layers.len();
    let mut select: Option<usize> = None;

    egui::ScrollArea::vertical()
        .max_height(190.0)
        .show(ui, |ui| {
            for index in (0..count).rev() {
                let Some(layer) = ed.layers.get_mut(index) else {
                    continue;
                };
                let is_active = index == active;

                ui.horizontal(|ui| {
                    // Visibility toggles independently of selection, so it must
                    // not be swallowed by the row's select handler.
                    let eye = if layer.visible { "◉" } else { "○" };
                    if ui
                        .add(egui::Button::new(eye).small().frame(false))
                        .on_hover_text("Show or hide")
                        .clicked()
                    {
                        layer.visible = !layer.visible;
                    }

                    let label = egui::RichText::new(layer.name.clone());
                    let label = if layer.visible { label } else { label.weak() };
                    if ui.selectable_label(is_active, label).clicked() {
                        select = Some(index);
                    }
                });
            }
        });

    if let Some(index) = select {
        ed.layers.set_active(index);
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(active + 1 < count, egui::Button::new("↑").small())
            .clicked()
        {
            actions.move_layer_up = Some(active);
        }
        if ui
            .add_enabled(active > 0, egui::Button::new("↓").small())
            .clicked()
        {
            actions.move_layer_down = Some(active);
        }
        // The last layer cannot go — a document needs somewhere to paint.
        if ui
            .add_enabled(count > 1, egui::Button::new("Delete").small())
            .on_hover_text("Deleting a layer clears undo history")
            .clicked()
        {
            actions.delete_layer = Some(active);
        }
    });

    ui.add_space(6.0);

    let layer = ed.layers.active_mut();
    ui.add(egui::Slider::new(&mut layer.opacity, 0.0..=1.0).text("Layer opacity"));

    egui::ComboBox::from_label("Blend")
        .selected_text(layer.blend.label())
        .show_ui(ui, |ui| {
            for mode in BlendMode::ALL {
                ui.selectable_value(&mut layer.blend, mode, mode.label());
            }
        });
}
