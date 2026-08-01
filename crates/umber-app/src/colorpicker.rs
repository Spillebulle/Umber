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

/// Smallest picker that is still a picker.
///
/// A docked panel can be dragged narrow enough that `available_width` reaches
/// zero, and every shape here is built from it. A ring whose inner radius went
/// negative drew a mesh turned inside out across the whole panel; a square with
/// a negative side is an `egui::Rect` with its max left of its min. Below this
/// the picker is drawn small and useless, which is honest, rather than wrong.
const MIN_PICKER: f32 = 48.0;

/// How wide a shape's edge is faded over, in points.
///
/// One physical pixel, which is what egui's own tessellator uses.
///
/// Every shape in this picker is a hand-built [`Mesh`] rather than a
/// [`Shape`] egui tessellates, because none of them is one flat colour and a
/// tessellated shape has exactly one. The cost of that is antialiasing: the
/// tessellator antialiases by extruding a shape's outline into a one-pixel
/// skirt that fades to nothing, and a mesh handed to it whole never goes
/// through that step. So the meshes here carry their own skirt. Without it the
/// hue ring's two circular edges and the triangle's three diagonals come out as
/// visible stair-steps.
fn feather(ui: &Ui) -> f32 {
    1.0 / ui.ctx().pixels_per_point().max(0.5)
}

/// The same colour with nothing left of it — the outer edge of a skirt.
///
/// Mesh vertex colours are premultiplied, so this is all zeroes and the
/// interpolation from the opaque vertex to it is a plain linear fade.
fn faded(colour: Color32) -> Color32 {
    Color32::from_rgba_unmultiplied(colour.r(), colour.g(), colour.b(), 0)
}

fn hue_colour(h: f32) -> Color32 {
    let [r, g, b, _] = Hsv::new(h, 1.0, 1.0).to_color(1.0).to_srgb_u8();
    Color32::from_rgb(r, g, b)
}

fn hsv_colour(h: f32, s: f32, v: f32) -> Color32 {
    let [r, g, b, _] = Hsv::new(h, s, v).to_color(1.0).to_srgb_u8();
    Color32::from_rgb(r, g, b)
}

/// Where the triangle's hue corner sits when it is not following the hue.
///
/// Straight up. Screen y is down, so that is a quarter turn anticlockwise from
/// egui's zero angle, which points right. Any orientation would do — the
/// barycentric maths reads the three corners wherever they are — but a fixed
/// triangle that points sideways looks like one that failed to finish turning.
const STILL_APEX: f32 = -std::f32::consts::FRAC_PI_2;

/// Draw the picker. Returns true when the colour changed.
pub fn show(
    ui: &mut Ui,
    p: &Palette,
    mode: PickerMode,
    shape: &mut WheelShape,
    rotate: &mut bool,
    hsv: &mut Hsv,
) -> bool {
    match mode {
        PickerMode::Wheel => wheel(ui, p, shape, rotate, hsv),
        PickerMode::Square => square(ui, p, hsv),
        PickerMode::Sliders => sliders(ui, p, hsv),
    }
}

fn wheel(
    ui: &mut Ui,
    p: &Palette,
    shape: &mut WheelShape,
    rotate: &mut bool,
    hsv: &mut Hsv,
) -> bool {
    let mut changed = false;

    let size = ui.available_width().clamp(MIN_PICKER, 176.0);
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width().max(size), size), Sense::hover());
    let area = Rect::from_center_size(rect.center(), vec2(size, size));
    let centre = area.center();
    let outer = size * 0.5;
    // The ring is a fixed thickness, so a small enough wheel would have its
    // inner edge outside its outer one. Keep a hub for the triangle instead.
    let inner = (outer - RING_THICKNESS).max(outer * 0.25);

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

    let f = feather(ui);
    // Enough segments that the facets are shorter than the feather is wide,
    // otherwise a smooth edge is drawn along a visibly polygonal outline. Scaled
    // to the wheel rather than fixed: a picker in a narrow panel does not need
    // the segments a wide one does, and a wide one needed more than the flat 96
    // this used to draw.
    let segments = ((outer * 0.9) as usize).clamp(RING_SEGMENTS, 320);

    let painter = ui.painter();
    let mut mesh = Mesh::default();
    // Four radii across the ring: a transparent skirt, the ring itself, and
    // another skirt. See `feather`.
    let radii = [
        ((inner - f).max(0.0), false),
        (inner, true),
        (outer, true),
        (outer + f, false),
    ];
    for i in 0..segments {
        let a0 = (i as f32 / segments as f32) * std::f32::consts::TAU;
        let a1 = ((i + 1) as f32 / segments as f32) * std::f32::consts::TAU;

        let base = mesh.vertices.len() as u32;
        for angle in [a0, a1] {
            let dir = vec2(angle.cos(), angle.sin());
            let colour = hue_colour(angle.to_degrees());
            for (radius, solid) in radii {
                mesh.vertices.push(Vertex {
                    pos: centre + dir * radius,
                    uv: egui::epaint::WHITE_UV,
                    color: if solid { colour } else { faded(colour) },
                });
            }
        }
        // Three bands between the four radii, each a quad spanning the segment.
        for band in 0..radii.len() as u32 - 1 {
            let l = base + band;
            let r = base + radii.len() as u32 + band;
            mesh.indices
                .extend_from_slice(&[l, l + 1, r, l + 1, r + 1, r]);
        }
    }
    painter.add(Shape::mesh(mesh));

    // Hue marker, on the middle of the ring wherever the ring ended up.
    let ha = hsv.h.to_radians();
    let marker = centre + vec2(ha.cos(), ha.sin()) * (inner + outer) * 0.5;
    painter.circle_stroke(marker, 6.0, Stroke::new(2.0, Color32::WHITE));

    // --- saturation / value shape ---
    match shape {
        WheelShape::Square => {
            // Largest square that fits inside the ring.
            let half = (inner * std::f32::consts::FRAC_1_SQRT_2 - 2.0).max(1.0);
            let sv = Rect::from_center_size(centre, vec2(half * 2.0, half * 2.0));
            changed |= sv_square(ui, sv, hsv, "wheel-sv");
        }
        WheelShape::Triangle => {
            changed |= sv_triangle(ui, centre, (inner - 3.0).max(1.0), *rotate, hsv);
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
            (WheelShape::Triangle, "Triangle"),
            (WheelShape::Square, "Square"),
        ],
    ) {
        *shape = picked;
    }

    // Only the triangle turns, so the row is drawn only when it is showing —
    // rather than drawn disabled — because the square centre has no orientation
    // for the setting to be about. A dead control here would be asking the user
    // to work out which of the two shapes it refers to.
    if *shape == WheelShape::Triangle {
        crate::widgets::toggle_row(ui, p, "Rotate with hue", rotate);
    }

    changed
}

/// Saturation/value triangle inscribed in the ring.
///
/// The apex is the full hue, with white and black at the other two corners.
/// When `rotate` is set the apex tracks the hue marker round the ring, which is
/// what the design shows; otherwise it holds still at [`STILL_APEX`] and only
/// its colour changes.
///
/// The choice is not cosmetic. Following the hue keeps the apex next to the
/// marker that sets it, so the two controls read as one instrument — but it
/// also means the whole saturation/value field swings under the pointer while
/// the ring is being dragged, so a tint chosen at one hue is somewhere else at
/// the next. Holding still gives up the first to get the second: the point you
/// last picked stays where you left it, and picking the same tint across
/// several hues becomes a matter of returning to the same place.
fn sv_triangle(ui: &mut Ui, centre: Pos2, radius: f32, rotate: bool, hsv: &mut Hsv) -> bool {
    let mut changed = false;
    let (hue_pt, white_pt, black_pt) = triangle_corners(centre, radius, rotate, hsv.h);

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

    let f = feather(ui);
    let corners = [
        (hue_pt, hue_colour(hsv.h)),
        (white_pt, Color32::WHITE),
        (black_pt, Color32::BLACK),
    ];
    // The centroid is the direction to push each corner out along for the
    // skirt. Exact for the ring, which is radial; for a triangle it makes the
    // skirt a fraction wider at the corners than along the edges, which at one
    // pixel nobody can see.
    let centroid = pos2(
        corners.iter().map(|(pt, _)| pt.x).sum::<f32>() / 3.0,
        corners.iter().map(|(pt, _)| pt.y).sum::<f32>() / 3.0,
    );

    let painter = ui.painter();
    let mut mesh = Mesh::default();
    // 0..3 is the triangle; 3..6 is the transparent skirt outside it. See
    // `feather` for why it is here rather than left to egui.
    for (pt, colour) in corners {
        mesh.vertices.push(Vertex {
            pos: pt,
            uv: egui::epaint::WHITE_UV,
            color: colour,
        });
    }
    for (pt, colour) in corners {
        let away = pt - centroid;
        // A triangle squashed to nothing has no outward direction, and
        // normalising it would put NaN into the mesh.
        let out = if away.length() > 1e-3 {
            pt + away.normalized() * f
        } else {
            pt
        };
        mesh.vertices.push(Vertex {
            pos: out,
            uv: egui::epaint::WHITE_UV,
            color: faded(colour),
        });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2]);
    for i in 0..3u32 {
        let j = (i + 1) % 3;
        mesh.indices
            .extend_from_slice(&[i, j, j + 3, i, j + 3, i + 3]);
    }
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

/// The triangle's three corners, in the order the rest of this file reads them:
/// full hue, white, black.
///
/// Split out from [`sv_triangle`] because it is the whole of what `rotate`
/// changes, and everything else about the triangle — the hit test, the mesh,
/// the marker — is derived from these three points. Testing it without a `Ui`
/// is therefore testing the feature.
fn triangle_corners(centre: Pos2, radius: f32, rotate: bool, hue: f32) -> (Pos2, Pos2, Pos2) {
    let base = if rotate { hue.to_radians() } else { STILL_APEX };
    let corner = |k: f32| {
        let a = base + k * std::f32::consts::TAU / 3.0;
        centre + vec2(a.cos(), a.sin()) * radius
    };
    (corner(0.0), corner(1.0), corner(2.0))
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
    let width = ui.available_width().max(MIN_PICKER);

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
            let width = (ui.available_width() - value_w - 8.0).max(MIN_PICKER * 0.5);
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

#[cfg(test)]
mod tests {
    use super::*;

    const CENTRE: Pos2 = pos2(100.0, 100.0);
    const RADIUS: f32 = 50.0;

    fn corners(rotate: bool, hue: f32) -> (Pos2, Pos2, Pos2) {
        triangle_corners(CENTRE, RADIUS, rotate, hue)
    }

    fn apart(a: Pos2, b: Pos2) -> f32 {
        (a - b).length()
    }

    #[test]
    fn a_rotating_triangle_follows_the_hue() {
        let (red, ..) = corners(true, 0.0);
        let (green, ..) = corners(true, 120.0);
        // A third of a turn round the ring is a third of a turn of the apex.
        assert!(apart(red, green) > RADIUS, "{red:?} vs {green:?}");
        // And a full turn brings it back to where it started.
        let (round_again, ..) = corners(true, 360.0);
        assert!(apart(red, round_again) < 1e-3);
    }

    #[test]
    fn a_still_triangle_holds_its_corners_at_every_hue() {
        let at_zero = corners(false, 0.0);
        for hue in [1.0, 47.0, 120.0, 210.0, 359.0] {
            let (h, w, b) = corners(false, hue);
            assert!(apart(h, at_zero.0) < 1e-4, "hue corner moved at {hue}");
            assert!(apart(w, at_zero.1) < 1e-4, "white corner moved at {hue}");
            assert!(apart(b, at_zero.2) < 1e-4, "black corner moved at {hue}");
        }
    }

    /// A fixed triangle that pointed sideways would look like one that failed
    /// to finish turning, so the apex is straight up. Screen y is down.
    #[test]
    fn a_still_triangle_points_up() {
        let (hue_pt, ..) = corners(false, 0.0);
        assert!((hue_pt.x - CENTRE.x).abs() < 1e-3, "{hue_pt:?}");
        assert!(hue_pt.y < CENTRE.y, "{hue_pt:?}");
    }

    /// Whichever way it is turned, the three corners have to stay a triangle:
    /// the barycentric hit test divides by its area, and a degenerate one
    /// returns NaN and freezes the picker.
    #[test]
    fn the_corners_are_never_collinear() {
        for rotate in [true, false] {
            for hue in [0.0, 30.0, 90.0, 180.0, 275.0, 359.9] {
                let (h, w, b) = corners(rotate, hue);
                let (a, _, _) = barycentric(CENTRE, h, w, b);
                assert!(a.is_finite(), "degenerate at rotate={rotate} hue={hue}");
                // The centre is inside, so every weight is positive.
                let (x, y, z) = barycentric(CENTRE, h, w, b);
                assert!(x > 0.0 && y > 0.0 && z > 0.0, "{x} {y} {z}");
            }
        }
    }
}
