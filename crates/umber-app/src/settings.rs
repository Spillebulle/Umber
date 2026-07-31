//! The settings dialog, from the design's Settings screen.
//!
//! Two tabs so far: Themes and Shortcuts. The design also sketches tabs for
//! input, performance and file handling; those wait on there being settings
//! worth putting in them.

use crate::editor::Editor;
use crate::icons::{self, Icon};
use crate::shortcuts;
use crate::theme::{Palette, ThemeKind, metrics, text};
use crate::widgets;
use egui::{Align2, FontId, Frame, Margin, Rect, Sense, Stroke, vec2};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsTab {
    Themes,
    Shortcuts,
}

/// Draw the dialog if it is open.
pub fn show(root: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    if !ed.ui.settings_open {
        return;
    }

    let response = egui::Modal::new(egui::Id::new("settings"))
        .frame(
            Frame::NONE
                .fill(p.popover)
                .stroke(Stroke::new(1.0, p.popover_border))
                .corner_radius(8)
                .inner_margin(Margin::same(18)),
        )
        .show(root.ctx(), |ui| {
            ui.set_width(460.0);

            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Settings")
                        .size(text::CONTROL)
                        .color(p.text_strong)
                        .strong(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (rect, close) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
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
            widgets::segmented(
                ui,
                p,
                &mut ed.ui.settings_tab,
                &[
                    (SettingsTab::Themes, "Themes"),
                    (SettingsTab::Shortcuts, "Shortcuts"),
                ],
            );
            ui.add_space(14.0);

            match ed.ui.settings_tab {
                SettingsTab::Themes => themes_tab(ui, p, ed),
                SettingsTab::Shortcuts => shortcuts_tab(ui, p),
            }
        });

    if response.should_close() {
        ed.ui.settings_open = false;
    }
}

fn themes_tab(ui: &mut egui::Ui, p: &Palette, ed: &mut Editor) {
    ui.label(
        egui::RichText::new("Theme")
            .size(text::SMALL)
            .color(p.text_dim),
    );
    ui.add_space(8.0);

    ui.horizontal(|ui| {
        for kind in ThemeKind::ALL {
            if theme_card(ui, p, kind, ed.ui.theme == kind) {
                ed.ui.theme = kind;
            }
        }
    });

    ui.add_space(16.0);
    ui.label(
        egui::RichText::new("Layout")
            .size(text::SMALL)
            .color(p.text_dim),
    );
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Left-handed layout")
                .size(text::SMALL)
                .color(p.text),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            widgets::toggle(ui, p, &mut ed.ui.left_handed);
        });
    });
    ui.label(
        egui::RichText::new("Puts the tool rail under your drawing hand.")
            .size(10.0)
            .color(p.text_dim),
    );
}

/// A miniature of the workspace in that theme, so the choice is visual rather
/// than a name you have to try to remember the look of.
fn theme_card(ui: &mut egui::Ui, p: &Palette, kind: ThemeKind, selected: bool) -> bool {
    let swatch = Palette::of(kind);
    let (rect, response) = ui.allocate_exact_size(vec2(132.0, 92.0), Sense::click());

    let painter = ui.painter();
    painter.rect_filled(rect, metrics::RADIUS_LARGE, swatch.window);
    painter.rect_stroke(
        rect,
        metrics::RADIUS_LARGE,
        Stroke::new(
            if selected { 2.0 } else { 1.0 },
            if selected { p.accent } else { p.border },
        ),
        egui::StrokeKind::Inside,
    );

    let inner = rect.shrink(8.0);
    // Menu bar.
    painter.rect_filled(
        Rect::from_min_size(inner.left_top(), vec2(inner.width(), 9.0)),
        2.0,
        swatch.chrome,
    );
    painter.rect_filled(
        Rect::from_min_size(inner.left_top() + vec2(2.0, 2.0), vec2(5.0, 5.0)),
        1.0,
        swatch.accent,
    );
    // Rail, canvas, dock.
    let body_top = inner.top() + 12.0;
    let body_h = inner.height() - 12.0 - 14.0;
    painter.rect_filled(
        Rect::from_min_size(inner.left_top() + vec2(0.0, 12.0), vec2(11.0, body_h)),
        2.0,
        swatch.chrome,
    );
    painter.rect_filled(
        Rect::from_min_size(
            egui::pos2(inner.left() + 13.0, body_top),
            vec2(inner.width() - 13.0 - 30.0, body_h),
        ),
        2.0,
        swatch.backdrop,
    );
    painter.rect_filled(
        Rect::from_min_size(
            egui::pos2(inner.right() - 28.0, body_top),
            vec2(28.0, body_h),
        ),
        2.0,
        swatch.dock,
    );
    // An accent bar standing in for a slider.
    painter.rect_filled(
        Rect::from_min_size(
            egui::pos2(inner.right() - 24.0, body_top + 8.0),
            vec2(14.0, 3.0),
        ),
        1.5,
        swatch.accent,
    );

    painter.text(
        egui::pos2(inner.left(), inner.bottom() - 4.0),
        Align2::LEFT_CENTER,
        kind.label(),
        FontId::proportional(text::SMALL),
        if selected { p.text_strong } else { p.text_dim },
    );

    response.clicked()
}

fn shortcuts_tab(ui: &mut egui::Ui, p: &Palette) {
    ui.label(
        egui::RichText::new("Rebinding is not implemented yet.")
            .size(10.0)
            .color(p.text_dim),
    );
    ui.add_space(8.0);

    egui::ScrollArea::vertical()
        .max_height(300.0)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let bindings = shortcuts::defaults();
            let mut current = "";
            for action in shortcuts::Action::ALL {
                let category = action.category();
                if category != current {
                    if !current.is_empty() {
                        ui.add_space(10.0);
                    }
                    current = category;
                    ui.label(
                        egui::RichText::new(category)
                            .size(text::SMALL)
                            .color(p.text_dim)
                            .strong(),
                    );
                    ui.add_space(2.0);
                }

                let keys: Vec<String> = bindings
                    .iter()
                    .filter(|b| b.action == action)
                    .map(|b| b.display())
                    .collect();

                let (rect, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
                let painter = ui.painter();
                painter.text(
                    rect.left_center(),
                    Align2::LEFT_CENTER,
                    action.label(),
                    FontId::proportional(text::SMALL),
                    p.text,
                );

                if keys.is_empty() {
                    painter.text(
                        rect.right_center(),
                        Align2::RIGHT_CENTER,
                        "unbound",
                        FontId::proportional(text::TINY),
                        p.text_dim,
                    );
                    continue;
                }

                // Keys sit in keycap-ish chips so they read as input, laid out
                // right to left so the first binding stays nearest the label.
                let mut right = rect.right();
                for label in keys.iter().rev() {
                    let font = FontId::monospace(text::TINY);
                    let width = painter
                        .layout_no_wrap(label.clone(), font.clone(), p.text)
                        .size()
                        .x
                        + 14.0;
                    let cap = Rect::from_min_size(
                        egui::pos2(right - width, rect.center().y - 9.0),
                        vec2(width, 18.0),
                    );
                    painter.rect_filled(cap, 4.0, p.window);
                    painter.rect_stroke(
                        cap,
                        4.0,
                        Stroke::new(1.0, p.border),
                        egui::StrokeKind::Inside,
                    );
                    painter.text(cap.center(), Align2::CENTER_CENTER, label, font, p.text);
                    right = cap.left() - 5.0;
                }
            }
        });
}
