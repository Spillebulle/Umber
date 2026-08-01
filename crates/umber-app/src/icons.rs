//! Vector icons.
//!
//! Everything here is drawn from primitives rather than taken from a font.
//!
//! The obvious alternative — Unicode glyphs like `🗑`, `◉`, `⇋` — works only
//! for as long as the UI font happens to carry them. A text face such as
//! Archivo carries none of those symbols, so they silently become blank boxes,
//! and platform fallback would render them at a different weight and size on
//! Windows, Linux and Android. Drawing them keeps the icon set consistent with
//! the stroke weight of the rest of the interface and independent of whatever
//! font is loaded.
//!
//! Icons are authored against a 24×24 box and scaled to whatever rect they are
//! given, so a 16 px and a 32 px instance are the same shape.

use egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, Vec2, vec2};

const BOX: f32 = 24.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    // Tools
    Brush,
    Eraser,
    Pan,
    Zoom,
    // Layers
    Plus,
    Trash,
    ChevronUp,
    ChevronDown,
    Eye,
    EyeOff,
    // Chrome
    Close,
    Pencil,
    Gear,
    Check,
    // Brush library
    Grid,
    Import,
    // About and updates
    /// An arrow leaving a box: this opens somewhere outside Umber.
    Link,
    /// An arrow onto a line: fetch this.
    Download,
    // Layout
    Grip,
    Corner,
    // Picker
    HalfCircle,
}

/// Draw `icon` centred in `rect`.
///
/// Stroke weight scales with the box so small icons stay legible and large
/// ones do not turn spindly.
pub fn draw(painter: &Painter, rect: Rect, icon: Icon, colour: Color32) {
    let size = rect.width().min(rect.height());
    if size <= 1.0 {
        return;
    }
    let scale = size / BOX;
    let origin = rect.center() - Vec2::splat(size * 0.5);
    let at = |x: f32, y: f32| origin + vec2(x * scale, y * scale);
    let stroke = Stroke::new((2.0 * scale).max(1.0), colour);
    let line = |a: Pos2, b: Pos2| painter.line_segment([a, b], stroke);
    let path = |pts: Vec<Pos2>| {
        painter.add(Shape::line(pts, stroke));
    };

    match icon {
        Icon::Brush => {
            line(at(6.0, 18.0), at(17.0, 7.0));
            painter.circle_filled(at(6.0, 18.0), 2.8 * scale, colour);
        }

        Icon::Eraser => {
            // A block tipped 35°, as the design draws it.
            path(rotated_rect(&at, 12.0, 12.0, 5.5, 7.0, 35.0));
        }

        Icon::Pan => {
            // Four-way arrow. A bare cross reads as "add", not "move", so the
            // heads are what make this legible at 18 px.
            line(at(12.0, 4.0), at(12.0, 20.0));
            line(at(4.0, 12.0), at(20.0, 12.0));
            for (tip, a, b) in [
                ((12.0, 4.0), (9.2, 7.0), (14.8, 7.0)),
                ((12.0, 20.0), (9.2, 17.0), (14.8, 17.0)),
                ((4.0, 12.0), (7.0, 9.2), (7.0, 14.8)),
                ((20.0, 12.0), (17.0, 9.2), (17.0, 14.8)),
            ] {
                line(at(tip.0, tip.1), at(a.0, a.1));
                line(at(tip.0, tip.1), at(b.0, b.1));
            }
        }

        Icon::Zoom => {
            painter.circle_stroke(at(10.0, 10.0), 5.5 * scale, stroke);
            line(at(14.2, 14.2), at(20.0, 20.0));
        }

        Icon::Plus => {
            line(at(12.0, 6.0), at(12.0, 18.0));
            line(at(6.0, 12.0), at(18.0, 12.0));
        }

        Icon::Trash => {
            line(at(5.0, 7.0), at(19.0, 7.0));
            path(vec![
                at(9.5, 7.0),
                at(9.5, 4.5),
                at(14.5, 4.5),
                at(14.5, 7.0),
            ]);
            path(vec![
                at(6.8, 7.0),
                at(7.8, 20.0),
                at(16.2, 20.0),
                at(17.2, 7.0),
            ]);
        }

        Icon::ChevronUp => path(vec![at(6.0, 15.0), at(12.0, 9.0), at(18.0, 15.0)]),
        Icon::ChevronDown => path(vec![at(6.0, 9.0), at(12.0, 15.0), at(18.0, 9.0)]),

        Icon::Eye => {
            path(eye_outline(&at));
            painter.circle_filled(at(12.0, 12.0), 2.6 * scale, colour);
        }

        Icon::EyeOff => {
            path(eye_outline(&at));
            line(at(5.0, 19.0), at(19.0, 5.0));
        }

        Icon::Close => {
            line(at(6.5, 6.5), at(17.5, 17.5));
            line(at(17.5, 6.5), at(6.5, 17.5));
        }

        Icon::Pencil => {
            path(vec![
                at(5.0, 19.0),
                at(6.2, 15.0),
                at(16.5, 4.7),
                at(19.3, 7.5),
                at(9.0, 17.8),
                at(5.0, 19.0),
            ]);
            line(at(14.5, 6.7), at(17.3, 9.5));
        }

        Icon::Gear => {
            painter.circle_stroke(at(12.0, 12.0), 4.0 * scale, stroke);
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::TAU / 8.0;
                let (s, c) = a.sin_cos();
                line(
                    at(12.0 + c * 6.4, 12.0 + s * 6.4),
                    at(12.0 + c * 9.2, 12.0 + s * 9.2),
                );
            }
        }

        Icon::Check => path(vec![at(5.0, 12.5), at(10.0, 17.5), at(19.0, 6.5)]),

        Icon::Grid => {
            // Four cells: "show me the whole set", against the single column
            // the Brushes panel has room for. Drawn as closed paths rather than
            // stroked rects so the corner radius matches the rest of the set.
            for (x, y) in [(4.5, 4.5), (13.0, 4.5), (4.5, 13.0), (13.0, 13.0)] {
                path(vec![
                    at(x, y),
                    at(x + 6.5, y),
                    at(x + 6.5, y + 6.5),
                    at(x, y + 6.5),
                    at(x, y),
                ]);
            }
        }

        Icon::Import => {
            // An arrow dropping into an open tray. The tray is what separates
            // this from a plain download mark: the file is coming *into* a
            // collection that already exists.
            line(at(12.0, 3.5), at(12.0, 14.5));
            path(vec![at(8.0, 10.5), at(12.0, 14.5), at(16.0, 10.5)]);
            path(vec![
                at(5.0, 15.0),
                at(5.0, 20.0),
                at(19.0, 20.0),
                at(19.0, 15.0),
            ]);
        }

        Icon::Link => {
            // A box with its top-right corner open, and an arrow leaving
            // through it. The gap is what stops this reading as "add to a
            // frame": the arrow has to be seen to be going *out*.
            path(vec![
                at(13.0, 5.0),
                at(5.0, 5.0),
                at(5.0, 19.0),
                at(19.0, 19.0),
                at(19.0, 11.0),
            ]);
            line(at(11.5, 12.5), at(19.0, 5.0));
            path(vec![at(13.0, 5.0), at(19.0, 5.0), at(19.0, 11.0)]);
        }

        Icon::Download => {
            // Distinct from `Import`, which drops into an open tray: this is a
            // plain arrow onto a closed line, because what it fetches is one
            // file rather than an addition to a collection.
            line(at(12.0, 4.0), at(12.0, 15.0));
            path(vec![at(7.5, 10.5), at(12.0, 15.0), at(16.5, 10.5)]);
            line(at(5.0, 19.5), at(19.0, 19.5));
        }

        Icon::Grip => {
            // Two columns of dots — the universal "drag me" mark. Drawn as
            // filled circles rather than the `⠿` braille glyph a text font
            // would have to carry.
            for row in 0..3 {
                let y = 8.0 + row as f32 * 4.0;
                painter.circle_filled(at(10.0, y), 1.1 * scale, colour);
                painter.circle_filled(at(14.0, y), 1.1 * scale, colour);
            }
        }

        Icon::Corner => {
            // Resize grip: stepped diagonals in the bottom-right corner.
            line(at(20.0, 10.0), at(10.0, 20.0));
            line(at(20.0, 15.0), at(15.0, 20.0));
        }

        Icon::HalfCircle => {
            painter.circle_stroke(at(12.0, 12.0), 7.0 * scale, stroke);
            // Left half filled, as a triangle fan.
            let centre = at(12.0, 12.0);
            let mut mesh = egui::Mesh::default();
            const STEPS: usize = 20;
            for k in 0..=STEPS {
                let a =
                    std::f32::consts::FRAC_PI_2 + k as f32 * std::f32::consts::PI / STEPS as f32;
                let (s, c) = a.sin_cos();
                mesh.colored_vertex(centre + vec2(c, s) * 7.0 * scale, colour);
            }
            mesh.colored_vertex(centre, colour);
            let hub = mesh.vertices.len() as u32 - 1;
            for k in 0..STEPS as u32 {
                mesh.indices.extend_from_slice(&[hub, k, k + 1]);
            }
            painter.add(Shape::mesh(mesh));
        }
    }
}

/// The lens shape of an eye, as two arcs meeting at the corners.
fn eye_outline(at: &impl Fn(f32, f32) -> Pos2) -> Vec<Pos2> {
    const STEPS: usize = 14;
    let mut pts = Vec::with_capacity(STEPS * 2 + 2);
    let lid = |sign: f32, t: f32| {
        let x = 3.5 + t * 17.0;
        // `abs` guards the same NaN trap as elsewhere: sin(PI) in f32 lands a
        // hair below zero, and these values feed a position, not a colour, but
        // a negative bulge would kink the outline.
        let bulge = (t * std::f32::consts::PI).sin().abs();
        (x, 12.0 + sign * bulge * 5.5)
    };
    for k in 0..=STEPS {
        let (x, y) = lid(-1.0, k as f32 / STEPS as f32);
        pts.push(at(x, y));
    }
    for k in 0..=STEPS {
        let (x, y) = lid(1.0, 1.0 - k as f32 / STEPS as f32);
        pts.push(at(x, y));
    }
    pts
}

/// Corners of a rectangle rotated about its centre, closed back to the start.
fn rotated_rect(
    at: &impl Fn(f32, f32) -> Pos2,
    cx: f32,
    cy: f32,
    half_w: f32,
    half_h: f32,
    degrees: f32,
) -> Vec<Pos2> {
    let (sin, cos) = degrees.to_radians().sin_cos();
    [
        (-half_w, -half_h),
        (half_w, -half_h),
        (half_w, half_h),
        (-half_w, half_h),
        (-half_w, -half_h),
    ]
    .iter()
    .map(|(dx, dy)| at(cx + dx * cos - dy * sin, cy + dx * sin + dy * cos))
    .collect()
}
