//! The colour picker from the design's Colour panel.
//!
//! Three of the design's five modes are implemented: a hue ring with a
//! triangle or square centre, a plain saturation/value square with a hue bar,
//! and RGB sliders. Palette and Harmony are not built.
//!
//! HSV is the picker's state, not the colour. Deriving hue from RGB each frame
//! would lose it whenever saturation or value hits zero — drag the value to
//! black and the hue would snap back to red on the way out.

use crate::theme::Palette;
use egui::{Color32, Mesh, Pos2, Rect, Sense, Shape, Stroke, Ui, epaint::Vertex, pos2, vec2};
use umber_core::Hsv;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerMode {
    Wheel,
    Square,
    Sliders,
}

impl PickerMode {
    pub const ALL: [PickerMode; 3] = [Self::Wheel, Self::Square, Self::Sliders];

    pub fn label(self) -> &'static str {
        match self {
            Self::Wheel => "Wheel",
            Self::Square => "Square",
            Self::Sliders => "Sliders",
        }
    }
}

/// The shape inside the hue ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelShape {
    Triangle,
    Square,
}

const RING_SEGMENTS: usize = 96;
const RING_THICKNESS: f32 = 20.0;

fn hue_colour(h: f32) -> Color32 {
    let [r, g, b, _] = Hsv::new(h, 1.0, 1.0).to_color(1.0).to_srgb_u8();
    Color32::from_rgb(r, g, b)
}

fn hsv_colour(h: f32, s: f32, v: f32) -> Color32 {
    let [r, g, b, _] = Hsv::new(h, s, v).to_color(1.0).to_srgb_u8();
    Color32::from_rgb(r, g, b)
}

/// Draw the picker. Returns true when the colour changed.
pub fn show(
    ui: &mut Ui,
    p: &Palette,
    mode: PickerMode,
    shape: &mut WheelShape,
    hsv: &mut Hsv,
) -> bool {
    match mode {
        PickerMode::Wheel => wheel(ui, p, shape, hsv),
        PickerMode::Square => square(ui, p, hsv),
        PickerMode::Sliders => sliders(ui, p, hsv),
    }
}

fn wheel(ui: &mut Ui, p: &Palette, shape: &mut WheelShape, hsv: &mut Hsv) -> bool {
    let mut changed = false;

    let size = ui.available_width().min(176.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), size), Sense::hover());
    let area = Rect::from_center_size(rect.center(), vec2(size, size));
    let centre = area.center();
    let outer = size * 0.5;
    let inner = outer - RING_THICKNESS;

    // --- hue ring ---
    let ring_response = ui.interact(area, ui.id().with("hue-ring"), Sense::click_and_drag());
    if (ring_response.dragged() || ring_response.clicked())
        && let Some(pos) = ring_response.interact_pointer_pos()
    {
        let d = pos - centre;
        let radius = d.length();
        // Only the ring itself steers hue; the middle belongs to the
        // saturation/value shape.
        if radius > inner * 0.92 {
            hsv.h = d.y.atan2(d.x).to_degrees().rem_euclid(360.0);
            changed = true;
        }
    }

    let painter = ui.painter();
    let mut mesh = Mesh::default();
    for i in 0..RING_SEGMENTS {
        let a0 = (i as f32 / RING_SEGMENTS as f32) * std::f32::consts::TAU;
        let a1 = ((i + 1) as f32 / RING_SEGMENTS as f32) * std::f32::consts::TAU;
        let c0 = hue_colour(a0.to_degrees());
        let c1 = hue_colour(a1.to_degrees());

        let base = mesh.vertices.len() as u32;
        for (angle, colour) in [(a0, c0), (a1, c1)] {
            let dir = vec2(angle.cos(), angle.sin());
            mesh.vertices.push(Vertex {
                pos: centre + dir * outer,
                uv: egui::epaint::WHITE_UV,
                color: colour,
            });
            mesh.vertices.push(Vertex {
                pos: centre + dir * inner,
                uv: egui::epaint::WHITE_UV,
                color: colour,
            });
        }
        mesh.indices
            .extend_from_slice(&[base, base + 1, base + 2, base + 1, base + 3, base + 2]);
    }
    painter.add(Shape::mesh(mesh));

    // Hue marker.
    let ha = hsv.h.to_radians();
    let marker = centre + vec2(ha.cos(), ha.sin()) * (inner + RING_THICKNESS * 0.5);
    painter.circle_stroke(marker, 6.0, Stroke::new(2.0, Color32::WHITE));

    // --- saturation / value shape ---
    match shape {
        WheelShape::Square => {
            // Largest square that fits inside the ring.
            let half = inner * std::f32::consts::FRAC_1_SQRT_2 - 2.0;
            let sv = Rect::from_center_size(centre, vec2(half * 2.0, half * 2.0));
            changed |= sv_square(ui, sv, hsv, "wheel-sv");
        }
        WheelShape::Triangle => {
            changed |= sv_triangle(ui, centre, inner - 3.0, hsv);
        }
    }

    ui.add_space(8.0);

    // Triangle / Square switch.
    let mut picked = *shape;
    if crate::widgets::segmented(
        ui,
        p,
        &mut picked,
        &[
            (WheelShape::Triangle, "▲ Triangle"),
            (WheelShape::Square, "■ Square"),
        ],
    ) {
        *shape = picked;
    }

    changed
}

/// Saturation/value triangle inscribed in the ring, rotated to follow the hue.
///
/// The apex sits at the current hue, with white and black at the other two
/// corners, so the triangle turns with the ring the way the design shows.
fn sv_triangle(ui: &mut Ui, centre: Pos2, radius: f32, hsv: &mut Hsv) -> bool {
    let mut changed = false;
    let base = hsv.h.to_radians();
    let corner = |k: f32| {
        let a = base + k * std::f32::consts::TAU / 3.0;
        centre + vec2(a.cos(), a.sin()) * radius
    };
    // 0 = full hue, 1 = white, 2 = black.
    let (hue_pt, white_pt, black_pt) = (corner(0.0), corner(1.0), corner(2.0));

    let rect = Rect::from_center_size(centre, vec2(radius * 2.0, radius * 2.0));
    let response = ui.interact(rect, ui.id().with("wheel-tri"), Sense::click_and_drag());
    if (response.dragged() || response.clicked())
        && let Some(pos) = response.interact_pointer_pos()
    {
        // Barycentric coordinates give saturation and value directly.
        let (a, b, c) = barycentric(pos, hue_pt, white_pt, black_pt);
        if a.is_finite() {
            let (a, b, c) = clamp_barycentric(a, b, c);
            let v = a + b;
            hsv.v = v.clamp(0.0, 1.0);
            hsv.s = if v > 1e-4 {
                (a / v).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let _ = c;
            changed = true;
        }
    }

    let painter = ui.painter();
    let mut mesh = Mesh::default();
    for (pt, colour) in [
        (hue_pt, hue_colour(hsv.h)),
        (white_pt, Color32::WHITE),
        (black_pt, Color32::BLACK),
    ] {
        mesh.vertices.push(Vertex {
            pos: pt,
            uv: egui::epaint::WHITE_UV,
            color: colour,
        });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2]);
    painter.add(Shape::mesh(mesh));

    // Marker: rebuild the point from the current saturation and value.
    let a = hsv.s * hsv.v;
    let b = hsv.v - a;
    let c = 1.0 - hsv.v;
    let marker = pos2(
        hue_pt.x * a + white_pt.x * b + black_pt.x * c,
        hue_pt.y * a + white_pt.y * b + black_pt.y * c,
    );
    painter.circle_stroke(marker, 5.0, Stroke::new(2.0, Color32::WHITE));

    changed
}

fn barycentric(p: Pos2, a: Pos2, b: Pos2, c: Pos2) -> (f32, f32, f32) {
    let v0 = b - a;
    let v1 = c - a;
    let v2 = p - a;
    let den = v0.x * v1.y - v1.x * v0.y;
    if den.abs() < 1e-6 {
        return (f32::NAN, f32::NAN, f32::NAN);
    }
    let v = (v2.x * v1.y - v1.x * v2.y) / den;
    let w = (v0.x * v2.y - v2.x * v0.y) / den;
    (1.0 - v - w, v, w)
}

/// Push a point inside the triangle rather than rejecting it, so dragging past
/// an edge slides along it instead of freezing the picker.
fn clamp_barycentric(a: f32, b: f32, c: f32) -> (f32, f32, f32) {
    let (a, b, c) = (a.max(0.0), b.max(0.0), c.max(0.0));
    let sum = a + b + c;
    if sum <= 1e-6 {
        (1.0, 0.0, 0.0)
    } else {
        (a / sum, b / sum, c / sum)
    }
}

/// Saturation/value square with a hue bar beneath.
fn square(ui: &mut Ui, _p: &Palette, hsv: &mut Hsv) -> bool {
    let mut changed = false;
    let width = ui.available_width();

    let (rect, _) = ui.allocate_exact_size(vec2(width, 130.0), Sense::hover());
    changed |= sv_square(ui, rect, hsv, "square-sv");

    ui.add_space(9.0);

    let (bar, response) = ui.allocate_exact_size(vec2(width, 11.0), Sense::click_and_drag());
    if (response.dragged() || response.clicked())
        && let Some(pos) = response.interact_pointer_pos()
    {
        let t = ((pos.x - bar.left()) / bar.width().max(1.0)).clamp(0.0, 1.0);
        hsv.h = t * 360.0;
        changed = true;
    }

    let painter = ui.painter();
    let mut mesh = Mesh::default();
    const STEPS: usize = 48;
    for i in 0..=STEPS {
        let t = i as f32 / STEPS as f32;
        let x = bar.left() + bar.width() * t;
        let colour = hue_colour(t * 360.0);
        mesh.vertices.push(Vertex {
            pos: pos2(x, bar.top()),
            uv: egui::epaint::WHITE_UV,
            color: colour,
        });
        mesh.vertices.push(Vertex {
            pos: pos2(x, bar.bottom()),
            uv: egui::epaint::WHITE_UV,
            color: colour,
        });
        if i > 0 {
            let b = (i as u32 - 1) * 2;
            mesh.indices
                .extend_from_slice(&[b, b + 1, b + 2, b + 1, b + 3, b + 2]);
        }
    }
    painter.add(Shape::mesh(mesh));

    let knob = pos2(bar.left() + bar.width() * (hsv.h / 360.0), bar.center().y);
    painter.circle_filled(knob, 6.5, hue_colour(hsv.h));
    painter.circle_stroke(knob, 6.5, Stroke::new(2.0, Color32::WHITE));

    changed
}

/// The saturation/value gradient: white→hue left to right, black bottom.
fn sv_square(ui: &mut Ui, rect: Rect, hsv: &mut Hsv, salt: &str) -> bool {
    let mut changed = false;

    let response = ui.interact(rect, ui.id().with(salt), Sense::click_and_drag());
    if (response.dragged() || response.clicked())
        && let Some(pos) = response.interact_pointer_pos()
    {
        hsv.s = ((pos.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
        hsv.v = 1.0 - ((pos.y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0);
        changed = true;
    }

    // Four-corner interpolation is not enough — saturation and value do not
    // multiply linearly — so the square is drawn as a small grid.
    const N: usize = 8;
    let painter = ui.painter();
    let mut mesh = Mesh::default();
    for iy in 0..=N {
        for ix in 0..=N {
            let s = ix as f32 / N as f32;
            let v = 1.0 - iy as f32 / N as f32;
            mesh.vertices.push(Vertex {
                pos: pos2(
                    rect.left() + rect.width() * s,
                    rect.top() + rect.height() * (1.0 - v),
                ),
                uv: egui::epaint::WHITE_UV,
                color: hsv_colour(hsv.h, s, v),
            });
        }
    }
    let stride = (N + 1) as u32;
    for iy in 0..N as u32 {
        for ix in 0..N as u32 {
            let i = iy * stride + ix;
            mesh.indices.extend_from_slice(&[
                i,
                i + 1,
                i + stride,
                i + 1,
                i + stride + 1,
                i + stride,
            ]);
        }
    }
    painter.add(Shape::mesh(mesh));

    let marker = pos2(
        rect.left() + rect.width() * hsv.s,
        rect.top() + rect.height() * (1.0 - hsv.v),
    );
    painter.circle_stroke(marker, 5.5, Stroke::new(2.0, Color32::WHITE));

    changed
}

/// R/G/B rows, each a gradient showing what moving that channel would do.
fn sliders(ui: &mut Ui, p: &Palette, hsv: &mut Hsv) -> bool {
    let mut changed = false;
    let mut rgb = hsv.to_color(1.0).to_srgb_u8();

    for (index, label) in ["R", "G", "B"].iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(*label)
                    .monospace()
                    .size(11.0)
                    .color(p.text_dim),
            );

            let value_w = 30.0;
            let width = (ui.available_width() - value_w - 8.0).max(20.0);
            let (bar, response) =
                ui.allocate_exact_size(vec2(width, 12.0), Sense::click_and_drag());

            if (response.dragged() || response.clicked())
                && let Some(pos) = response.interact_pointer_pos()
            {
                let t = ((pos.x - bar.left()) / bar.width().max(1.0)).clamp(0.0, 1.0);
                rgb[index] = (t * 255.0).round() as u8;
                changed = true;
            }

            let ends = |v: u8| {
                let mut c = rgb;
                c[index] = v;
                Color32::from_rgb(c[0], c[1], c[2])
            };
            let painter = ui.painter();
            let mut mesh = Mesh::default();
            mesh.colored_vertex(bar.left_top(), ends(0));
            mesh.colored_vertex(bar.left_bottom(), ends(0));
            mesh.colored_vertex(bar.right_top(), ends(255));
            mesh.colored_vertex(bar.right_bottom(), ends(255));
            mesh.indices.extend_from_slice(&[0, 1, 2, 1, 3, 2]);
            painter.add(Shape::mesh(mesh));

            let t = rgb[index] as f32 / 255.0;
            let knob = pos2(bar.left() + bar.width() * t, bar.center().y);
            painter.circle_filled(knob, 7.0, Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
            painter.circle_stroke(knob, 7.0, Stroke::new(2.0, Color32::WHITE));

            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(format!("{}", rgb[index]))
                    .monospace()
                    .size(10.5)
                    .color(p.text),
            );
        });
    }

    if changed {
        // Keep the existing hue when the new colour is a grey, which has none.
        let next = umber_core::Color::from_srgb_u8(rgb[0], rgb[1], rgb[2], 255).to_hsv();
        hsv.s = next.s;
        hsv.v = next.v;
        if next.s > 1e-4 {
            hsv.h = next.h;
        }
    }

    changed
}
