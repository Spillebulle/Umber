//! Widgets drawn to match the design.
//!
//! egui's stock slider, checkbox and radio group have a look of their own that
//! the Graphite design does not use — thin rails with a round knob, pill
//! toggles, segmented pickers. These are painted directly rather than fought
//! with via styling.

use crate::theme::{Palette, metrics, text};
use egui::{Align2, Color32, FontId, Rect, Response, Sense, Stroke, Ui, Vec2, pos2, vec2};
use std::ops::RangeInclusive;

/// Label on the left, monospace readout on the right, thin rail beneath.
///
/// Returns true when the value changed. `log` maps the rail logarithmically,
/// which is what makes a 1–400 px brush size usable — half the travel covers
/// 1–20 px, where the useful sizes actually live.
pub fn slider_row(
    ui: &mut Ui,
    p: &Palette,
    label: &str,
    value: &mut f32,
    range: RangeInclusive<f32>,
    log: bool,
    display: impl Fn(f32) -> String,
) -> bool {
    let (lo, hi) = (*range.start(), *range.end());
    let log = log && lo > 0.0 && hi > lo;

    let to_t = |v: f32| {
        let v = v.clamp(lo, hi);
        if log {
            (v.ln() - lo.ln()) / (hi.ln() - lo.ln())
        } else {
            (v - lo) / (hi - lo)
        }
    };
    let from_t = |t: f32| {
        let t = t.clamp(0.0, 1.0);
        if log {
            (lo.ln() + t * (hi.ln() - lo.ln())).exp()
        } else {
            lo + t * (hi - lo)
        }
    };

    let mut changed = false;

    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;

        let width = ui.available_width();

        // Header: name and current value on one baseline.
        let (header, _) = ui.allocate_exact_size(vec2(width, text::SMALL + 2.0), Sense::hover());
        let painter = ui.painter();
        painter.text(
            header.left_center(),
            Align2::LEFT_CENTER,
            label,
            FontId::proportional(text::SMALL),
            p.text_dim,
        );
        painter.text(
            header.right_center(),
            Align2::RIGHT_CENTER,
            display(*value),
            FontId::monospace(text::TINY),
            p.text,
        );

        // Rail: a tall invisible hit area around a thin visible track, so the
        // 3 px rail is still comfortable to grab.
        let (row, response) =
            ui.allocate_exact_size(vec2(width, metrics::SLIDER_ROW), Sense::click_and_drag());
        let track = Rect::from_center_size(
            row.center(),
            vec2(row.width() - metrics::SLIDER_KNOB, metrics::SLIDER_RAIL),
        );

        if (response.dragged() || response.clicked())
            && let Some(pos) = response.interact_pointer_pos()
        {
            let t = ((pos.x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0);
            let next = from_t(t);
            if next != *value {
                *value = next;
                changed = true;
            }
        }

        let t = to_t(*value);
        let painter = ui.painter();
        let radius = metrics::SLIDER_RAIL * 0.5;
        painter.rect_filled(track, radius, p.rail);
        if t > 0.0 {
            let filled = Rect::from_min_size(track.min, vec2(track.width() * t, track.height()));
            painter.rect_filled(filled, radius, p.accent);
        }
        let knob = pos2(track.left() + track.width() * t, track.center().y);
        painter.circle_filled(knob, metrics::SLIDER_KNOB * 0.5, p.knob);
    });

    changed
}

/// Pill toggle, 28×16 with a sliding knob.
pub fn toggle(ui: &mut Ui, p: &Palette, on: &mut bool) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(vec2(28.0, 16.0), Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }

    let painter = ui.painter();
    painter.rect_filled(rect, 8.0, if *on { p.accent } else { p.rail });
    let knob_x = if *on {
        rect.right() - 2.0 - 6.0
    } else {
        rect.left() + 2.0 + 6.0
    };
    painter.circle_filled(pos2(knob_x, rect.center().y), 6.0, p.knob);
    response
}

/// A row of mutually exclusive choices inside an inset well.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    p: &Palette,
    current: &mut T,
    options: &[(T, &str)],
) -> bool {
    if options.is_empty() {
        return false;
    }
    let mut changed = false;

    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, 24.0), Sense::hover());
    ui.painter().rect_filled(rect, metrics::RADIUS, p.window);

    let inner = rect.shrink(2.0);
    let cell_w = inner.width() / options.len() as f32;

    for (i, (value, label)) in options.iter().enumerate() {
        let cell = Rect::from_min_size(
            pos2(inner.left() + cell_w * i as f32, inner.top()),
            vec2(cell_w, inner.height()),
        );
        let response = ui.interact(cell, ui.id().with((label, i)), Sense::click());
        if response.clicked() {
            *current = *value;
            changed = true;
        }

        let selected = *current == *value;
        let painter = ui.painter();
        if selected {
            painter.rect_filled(cell, metrics::RADIUS - 1.0, p.control_hover);
        } else if response.hovered() {
            painter.rect_filled(cell, metrics::RADIUS - 1.0, p.control);
        }
        painter.text(
            cell.center(),
            Align2::CENTER_CENTER,
            *label,
            FontId::proportional(text::TINY),
            if selected { p.text_strong } else { p.text_dim },
        );
    }

    changed
}

/// Full-width pills, used for Paint / Erase.
pub fn pills<T: PartialEq + Copy>(
    ui: &mut Ui,
    p: &Palette,
    current: &mut T,
    options: &[(T, &str)],
) -> bool {
    let mut changed = false;
    let gap = 6.0;
    let width = ui.available_width();
    let cell_w = (width - gap * (options.len() as f32 - 1.0)) / options.len() as f32;
    let (rect, _) = ui.allocate_exact_size(vec2(width, 26.0), Sense::hover());

    for (i, (value, label)) in options.iter().enumerate() {
        let cell = Rect::from_min_size(
            pos2(rect.left() + (cell_w + gap) * i as f32, rect.top()),
            vec2(cell_w, rect.height()),
        );
        let response = ui.interact(cell, ui.id().with((label, i)), Sense::click());
        if response.clicked() {
            *current = *value;
            changed = true;
        }

        let selected = *current == *value;
        let fill = if selected {
            p.control_active
        } else if response.hovered() {
            p.control_hover
        } else {
            p.control
        };
        let painter = ui.painter();
        painter.rect_filled(cell, metrics::RADIUS, fill);
        painter.text(
            cell.center(),
            Align2::CENTER_CENTER,
            *label,
            FontId::proportional(text::CONTROL),
            if selected { p.accent } else { p.text_muted },
        );
    }

    changed
}

/// Tabs with an accent underline on the active one.
pub fn tabs<T: PartialEq + Copy>(
    ui: &mut Ui,
    p: &Palette,
    current: &mut T,
    options: &[(T, &str)],
) -> bool {
    let mut changed = false;
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, 34.0), Sense::hover());
    let cell_w = rect.width() / options.len() as f32;

    ui.painter().line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0, p.border),
    );

    for (i, (value, label)) in options.iter().enumerate() {
        let cell = Rect::from_min_size(
            pos2(rect.left() + cell_w * i as f32, rect.top()),
            vec2(cell_w, rect.height()),
        );
        let response = ui.interact(cell, ui.id().with(("tab", label, i)), Sense::click());
        if response.clicked() {
            *current = *value;
            changed = true;
        }

        let selected = *current == *value;
        let painter = ui.painter();
        painter.text(
            cell.center(),
            Align2::CENTER_CENTER,
            *label,
            FontId::proportional(text::CONTROL),
            if selected {
                p.text_strong
            } else if response.hovered() {
                p.text
            } else {
                p.text_dim
            },
        );
        if selected {
            let underline = Rect::from_min_size(
                pos2(cell.left(), cell.bottom() - 2.0),
                vec2(cell.width(), 2.0),
            );
            painter.rect_filled(underline, 0.0, p.accent);
        }
    }

    changed
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolIcon {
    Brush,
    Eraser,
    Pan,
    Zoom,
}

/// A 36×36 icon button for the tool rail.
///
/// Icons are painted rather than loaded: the design specifies them as a handful
/// of SVG primitives, and drawing those directly avoids shipping an image
/// atlas or a font just for four glyphs.
pub fn tool_button(
    ui: &mut Ui,
    p: &Palette,
    icon: ToolIcon,
    active: bool,
    tooltip: &str,
) -> Response {
    let size = metrics::TOOL_BUTTON;
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());

    let fill = if active {
        p.control_active
    } else if response.hovered() {
        p.control_hover
    } else {
        Color32::TRANSPARENT
    };
    let painter = ui.painter();
    if fill != Color32::TRANSPARENT {
        painter.rect_filled(rect, metrics::RADIUS_LARGE, fill);
    }

    let colour = if active { p.accent } else { p.text_muted };
    draw_icon(painter, rect, icon, colour);

    response.on_hover_text(tooltip)
}

/// Icons are authored against an 18×18 viewBox, matching the design's SVGs,
/// and drawn at 1:1 inside the 36×36 button.
fn draw_icon(painter: &egui::Painter, rect: Rect, icon: ToolIcon, colour: Color32) {
    const BOX: f32 = 18.0;
    let origin = rect.center() - Vec2::splat(BOX * 0.5);
    let at = |x: f32, y: f32| origin + vec2(x, y);
    let stroke = Stroke::new(2.0, colour);

    match icon {
        ToolIcon::Brush => {
            painter.line_segment([at(4.0, 14.0), at(13.0, 5.0)], stroke);
            painter.circle_filled(at(4.0, 14.0), 2.4, colour);
        }
        ToolIcon::Eraser => {
            // A rounded rect rotated 35° about the centre. Painted as four
            // segments; the design's 2 px corner radius is below the threshold
            // where it reads at this size.
            let (cx, cy) = (9.0, 9.0);
            let angle = 35.0f32.to_radians();
            let (sin, cos) = angle.sin_cos();
            let corners = [(5.0, 4.0), (13.0, 4.0), (13.0, 14.0), (5.0, 14.0)];
            let pts: Vec<_> = corners
                .iter()
                .map(|(x, y)| {
                    let (dx, dy) = (x - cx, y - cy);
                    at(cx + dx * cos - dy * sin, cy + dx * sin + dy * cos)
                })
                .collect();
            for i in 0..4 {
                painter.line_segment([pts[i], pts[(i + 1) % 4]], stroke);
            }
        }
        ToolIcon::Pan => {
            painter.line_segment([at(9.0, 3.0), at(9.0, 15.0)], stroke);
            painter.line_segment([at(3.0, 9.0), at(15.0, 9.0)], stroke);
        }
        ToolIcon::Zoom => {
            painter.circle_stroke(at(8.0, 8.0), 4.5, stroke);
            painter.line_segment([at(11.5, 11.5), at(15.0, 15.0)], stroke);
        }
    }
}

/// A collapsible section heading with a disclosure arrow.
pub fn section(ui: &mut Ui, p: &Palette, label: &str, open: &mut bool, badge: Option<&str>) {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, 18.0), Sense::click());
    if response.clicked() {
        *open = !*open;
    }

    let painter = ui.painter();
    painter.text(
        rect.left_center() + vec2(2.0, 0.0),
        Align2::LEFT_CENTER,
        if *open { "▾" } else { "▸" },
        FontId::proportional(9.0),
        p.text_dim,
    );
    painter.text(
        rect.left_center() + vec2(16.0, 0.0),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(text::CONTROL),
        if response.hovered() {
            p.text_strong
        } else {
            p.text
        },
    );
    if let Some(badge) = badge {
        painter.text(
            rect.right_center(),
            Align2::RIGHT_CENTER,
            badge,
            FontId::proportional(10.0),
            p.text_dim,
        );
    }
}

/// A flat text button sized to fill its share of a row.
pub fn flat_button(ui: &mut Ui, p: &Palette, label: &str, width: f32, enabled: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 26.0), Sense::click());
    let response = if enabled {
        response
    } else {
        // Still consumes the space, but reports no clicks.
        ui.interact(rect, ui.id().with(("disabled", label)), Sense::hover())
    };

    let fill = if enabled && response.hovered() {
        p.control_hover
    } else {
        p.control
    };
    let painter = ui.painter();
    painter.rect_filled(rect, metrics::RADIUS, fill);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(text::SMALL),
        if !enabled {
            p.text_dim.gamma_multiply(0.5)
        } else if response.hovered() {
            p.text_strong
        } else {
            p.text_muted
        },
    );
    response
}
