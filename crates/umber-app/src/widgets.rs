//! Widgets drawn to match the design.
//!
//! egui's stock slider, checkbox and radio group have a look of their own that
//! the Graphite design does not use — thin rails with a round knob, pill
//! toggles, segmented pickers. These are painted directly rather than fought
//! with via styling.

use crate::theme::{Palette, metrics, text};
use egui::{Align2, Color32, FontId, Rect, Response, Sense, Stroke, Ui, Vec2, pos2, vec2};
use std::ops::RangeInclusive;
use umber_core::ResponseCurve;

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

/// Map a value onto `0..=1` along a slider, linearly or logarithmically.
///
/// A logarithmic map is what makes a 1–400 px brush size usable: half the
/// travel covers 1–20 px, where the useful sizes actually live.
fn to_t(v: f32, lo: f32, hi: f32, log: bool) -> f32 {
    let v = v.clamp(lo, hi);
    if log {
        (v.ln() - lo.ln()) / (hi.ln() - lo.ln())
    } else {
        (v - lo) / (hi - lo)
    }
}

fn from_t(t: f32, lo: f32, hi: f32, log: bool) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if log {
        (lo.ln() + t * (hi.ln() - lo.ln())).exp()
    } else {
        lo + t * (hi - lo)
    }
}

/// Drag a track and report whether the value moved.
fn drag_track(
    response: &Response,
    track: Rect,
    value: &mut f32,
    lo: f32,
    hi: f32,
    log: bool,
) -> bool {
    if !(response.dragged() || response.clicked()) {
        return false;
    }
    let Some(pos) = response.interact_pointer_pos() else {
        return false;
    };
    let t = ((pos.x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0);
    let next = from_t(t, lo, hi, log);
    if next == *value {
        return false;
    }
    *value = next;
    true
}

fn paint_track(painter: &egui::Painter, p: &Palette, track: Rect, t: f32, knob: f32) {
    let radius = track.height() * 0.5;
    painter.rect_filled(track, radius, p.rail);
    if t > 0.0 {
        let filled = Rect::from_min_size(track.min, vec2(track.width() * t, track.height()));
        painter.rect_filled(filled, radius, p.accent);
    }
    if knob > 0.0 {
        painter.circle_filled(
            pos2(track.left() + track.width() * t, track.center().y),
            knob * 0.5,
            p.knob,
        );
    }
}

/// Compact label + rail + readout, laid out horizontally for the options strip.
pub fn inline_slider(
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

    ui.label(
        egui::RichText::new(label)
            .size(text::SMALL)
            .color(p.text_dim),
    );

    let (row, response) = ui.allocate_exact_size(vec2(90.0, 16.0), Sense::click_and_drag());
    let track = Rect::from_center_size(row.center(), vec2(row.width() - 10.0, 3.0));
    let changed = drag_track(&response, track, value, lo, hi, log);
    paint_track(ui.painter(), p, track, to_t(*value, lo, hi, log), 10.0);

    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(display(*value))
            .monospace()
            .size(text::TINY)
            .color(p.text),
    );

    changed
}

/// A rail with no label or readout, for rows that supply their own.
pub fn bare_slider(ui: &mut Ui, p: &Palette, value: &mut f32, range: RangeInclusive<f32>) -> bool {
    let (lo, hi) = (*range.start(), *range.end());
    let width = (ui.available_width() - 30.0).max(24.0);
    let (row, response) = ui.allocate_exact_size(vec2(width, 14.0), Sense::click_and_drag());
    let track = Rect::from_center_size(row.center(), vec2(row.width(), 3.0));
    let changed = drag_track(&response, track, value, lo, hi, false);
    paint_track(ui.painter(), p, track, to_t(*value, lo, hi, false), 0.0);
    changed
}

/// A read-only bordered pill showing a name and its value.
pub fn chip(ui: &mut Ui, p: &Palette, label: &str, value: &str) {
    let padding = 9.0;
    let font = FontId::proportional(text::SMALL);
    let text_w = ui
        .painter()
        .layout_no_wrap(format!("{label}  {value}"), font.clone(), p.text)
        .size()
        .x;
    let (rect, _) = ui.allocate_exact_size(vec2(text_w + padding * 2.0, 22.0), Sense::hover());

    let painter = ui.painter();
    painter.rect_filled(rect, metrics::RADIUS, p.window);
    painter.rect_stroke(
        rect,
        metrics::RADIUS,
        Stroke::new(1.0, p.border),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.left_center() + vec2(padding, 0.0),
        Align2::LEFT_CENTER,
        label,
        font.clone(),
        p.text_dim,
    );
    painter.text(
        rect.right_center() - vec2(padding, 0.0),
        Align2::RIGHT_CENTER,
        value,
        font,
        p.text,
    );
}

/// A brush preset: a tapered stroke sample, then the name.
///
/// The sample is drawn from the preset's own opacity and hardness, so the rows
/// differ the way the brushes do rather than all showing the same smear.
pub fn brush_preset_row(
    ui: &mut Ui,
    p: &Palette,
    name: &str,
    opacity: f32,
    hardness: f32,
    selected: bool,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());

    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, metrics::RADIUS, p.control_active);
    } else if response.hovered() {
        painter.rect_filled(rect, metrics::RADIUS, p.control);
    }

    // Tapered sample: a row of circles whose radius and alpha rise then fall.
    let sample = Rect::from_min_size(rect.left_top() + vec2(7.0, 6.0), vec2(64.0, 14.0));
    const STEPS: usize = 26;
    for i in 0..STEPS {
        let t = i as f32 / (STEPS - 1) as f32;
        // Ends taper to nothing; the middle is the brush at full width.
        //
        // The `max(0.0)` is load-bearing: `sin(PI)` in f32 lands just *below*
        // zero, and a negative base with a fractional exponent is NaN, which
        // propagates into the alpha and trips ecolor's assert.
        let taper = (t * std::f32::consts::PI).sin().max(0.0);
        let radius = (sample.height() * 0.5) * taper.powf(0.6);
        if radius <= 0.2 {
            continue;
        }
        // Softer brushes read as a wider, fainter smear.
        let alpha = opacity * taper.powf(1.0 + (1.0 - hardness) * 1.5);
        painter.circle_filled(
            pos2(sample.left() + sample.width() * t, sample.center().y),
            radius,
            p.text_strong.gamma_multiply(alpha.clamp(0.0, 1.0)),
        );
    }

    painter.text(
        pos2(sample.right() + 9.0, rect.center().y),
        Align2::LEFT_CENTER,
        name,
        FontId::proportional(text::TINY),
        if selected { p.text_strong } else { p.text },
    );

    response
}

pub struct LayerRowResponse {
    pub clicked: bool,
    pub eye_clicked: bool,
}

/// One row of the layer stack: visibility, a thumbnail chip, name and blend.
pub fn layer_row(
    ui: &mut Ui,
    p: &Palette,
    name: &str,
    visible: bool,
    active: bool,
    blend: &str,
) -> LayerRowResponse {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());

    let painter = ui.painter();
    if active {
        painter.rect_filled(rect, metrics::RADIUS, p.control_active);
        painter.rect_stroke(
            rect,
            metrics::RADIUS,
            Stroke::new(1.0, p.accent_dim),
            egui::StrokeKind::Inside,
        );
    } else if response.hovered() {
        painter.rect_filled(rect, metrics::RADIUS, p.control);
    }

    // The eye is its own hit target inside the row, so toggling visibility
    // does not also change the selection.
    let eye = Rect::from_min_size(rect.left_top() + vec2(5.0, 6.0), vec2(18.0, 18.0));
    let eye_response = ui.interact(eye, ui.id().with(("eye", name)), Sense::click());

    let painter = ui.painter();
    painter.text(
        eye.center(),
        Align2::CENTER_CENTER,
        if visible { "◉" } else { "○" },
        FontId::proportional(text::SMALL),
        if visible { p.text } else { p.text_dim },
    );

    // Thumbnail placeholder: a checker chip. Rendering real layer thumbnails
    // needs a downscale pass that does not exist yet.
    let thumb = Rect::from_min_size(rect.left_top() + vec2(27.0, 3.0), vec2(24.0, 24.0));
    painter.rect_filled(thumb, 3.0, p.window);
    for i in 0..4 {
        for j in 0..4 {
            if (i + j) % 2 == 0 {
                continue;
            }
            let cell = Rect::from_min_size(
                thumb.left_top() + vec2(i as f32 * 6.0, j as f32 * 6.0),
                vec2(6.0, 6.0),
            );
            painter.rect_filled(cell.intersect(thumb), 0.0, p.control_hover);
        }
    }

    painter.text(
        pos2(thumb.right() + 8.0, rect.center().y),
        Align2::LEFT_CENTER,
        name,
        FontId::proportional(text::SMALL),
        match (active, visible) {
            (true, _) => p.text_strong,
            (false, true) => p.text,
            (false, false) => p.text_dim,
        },
    );
    painter.text(
        rect.right_center() - vec2(7.0, 0.0),
        Align2::RIGHT_CENTER,
        blend,
        FontId::proportional(9.0),
        p.text_dim.gamma_multiply(0.8),
    );

    LayerRowResponse {
        clicked: response.clicked(),
        eye_clicked: eye_response.clicked(),
    }
}

/// A draggable response curve.
///
/// Handles move vertically only — their inputs are fixed and evenly spaced —
/// so the curve cannot be dragged into a shape that maps one pressure to two
/// values.
pub fn curve_editor(ui: &mut Ui, p: &Palette, curve: &mut ResponseCurve, size: f32) -> bool {
    let mut changed = false;
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click_and_drag());

    let at = |i: usize, v: f32| {
        pos2(
            rect.left() + rect.width() * ResponseCurve::x_of(i),
            rect.bottom() - rect.height() * v,
        )
    };

    // Drag whichever handle is nearest horizontally — with five fixed columns
    // that is unambiguous, and it means you never have to hit the dot exactly.
    if (response.dragged() || response.clicked())
        && let Some(pos) = response.interact_pointer_pos()
    {
        let t = ((pos.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
        let i = (t * (ResponseCurve::N - 1) as f32).round() as usize;
        let v = 1.0 - ((pos.y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0);
        let before = curve.points[i.min(ResponseCurve::N - 1)];
        curve.set(i, v);
        changed = (curve.points[i.min(ResponseCurve::N - 1)] - before).abs() > 1e-5;
    }

    let painter = ui.painter();
    painter.rect_filled(rect, metrics::RADIUS, p.window);
    painter.rect_stroke(
        rect,
        metrics::RADIUS,
        Stroke::new(1.0, p.border),
        egui::StrokeKind::Inside,
    );

    // Quarter grid, plus the diagonal as a reference for "no change".
    for k in 1..4 {
        let f = k as f32 / 4.0;
        let x = rect.left() + rect.width() * f;
        let y = rect.top() + rect.height() * f;
        painter.line_segment(
            [pos2(x, rect.top()), pos2(x, rect.bottom())],
            Stroke::new(1.0, p.border.gamma_multiply(0.6)),
        );
        painter.line_segment(
            [pos2(rect.left(), y), pos2(rect.right(), y)],
            Stroke::new(1.0, p.border.gamma_multiply(0.6)),
        );
    }
    painter.line_segment(
        [rect.left_bottom(), rect.right_top()],
        Stroke::new(1.0, p.border),
    );

    let points: Vec<_> = (0..ResponseCurve::N)
        .map(|i| at(i, curve.points[i]))
        .collect();
    painter.add(egui::Shape::line(
        points.clone(),
        Stroke::new(2.0, p.accent),
    ));
    for point in points {
        painter.circle_filled(point, 4.0, p.knob);
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
