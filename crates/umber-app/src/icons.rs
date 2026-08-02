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
    /// A dashed box — the marquee, which is what a selection looks like on the
    /// canvas.
    Select,
    /// A box with its corner handles — what a floating transform looks like on
    /// the canvas, as `Select` is what a selection looks like.
    Transform,
    Pan,
    Zoom,
    // The transform tool's own marks, drawn over the canvas beside the box.
    /// An arrow curving round: drag outside the box to turn it.
    Rotate,
    /// Two shapes either side of a dashed axis, mirrored left to right.
    FlipHorizontal,
    /// The same, mirrored top to bottom.
    FlipVertical,
    // Layers
    Plus,
    Trash,
    ChevronUp,
    ChevronDown,
    Eye,
    EyeOff,
    /// A frame with a disc in it: the layer, and the coverage that hides part
    /// of it. Drawn as an outline plus a solid shape rather than as two
    /// outlines, because at 16 px two nested rings read as a target.
    Mask,
    /// An arrow turning down and to the left, over a rule — the mark every
    /// application uses for "bounded by the layer below".
    Clip,
    /// A closed padlock.
    Lock,
    /// The same padlock with its shackle open. A second icon rather than the
    /// first drawn dim: dim means "unavailable" everywhere else in the
    /// interface, and a lock that is merely *off* is very much available.
    Unlock,
    /// Two chain links: these layers move together.
    Chain,
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
    // History
    /// A sheet with its corner turned down: the document itself, as opposed to
    /// something done to it.
    Document,
    // Making a brush. Added at the end deliberately — this enum is shared, and
    // renumbering it would be a merge that compiles and draws the wrong marks.
    /// A brush with a plus beside it: make a brush from nothing, as against
    /// `Plus` alone, which everywhere else means "save what is in your hand".
    BrushNew,
    /// A triangle with a bar and a dot in it — the crash box's mark, and the
    /// only place in the interface that something has gone irrecoverably wrong.
    /// A triangle rather than a circle: a circled `i` is information and a
    /// circled `!` is a warning somebody can carry on past, and this is
    /// neither.
    Alert,
    // The selection's own strip of controls, drawn over the canvas beside a
    // marquee. Added at the end for the reason `BrushNew` was.
    /// Two sheets, one behind the other: take a copy and leave the original.
    Copy,
    /// A pair of scissors: take it and leave nothing.
    Cut,
    /// `Select`'s dashed box with a stroke through it — not the box greyed, and
    /// not `Close`'s cross: this clears one specific thing, and the mark has to
    /// say *which*.
    Deselect,
    // Layer folders. At the end for the reason the brush marks above are: this
    // enum is shared, and renumbering it would be a merge that compiles and
    // draws the wrong marks.
    /// A folder with a tab, as a layer group's row mark. Drawn where a layer's
    /// row draws its thumbnail — a folder has no picture of its own, and one of
    /// an arbitrary child would be a picture that lies about what is inside.
    Folder,
    /// A chevron pointing right: this folder is shut. Its pair is
    /// [`Icon::ChevronDown`], which already exists and already points the way a
    /// disclosure open should.
    ChevronRight,
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

        Icon::Select => {
            // Dashes rather than a stroked rectangle: a solid box reads as a
            // shape tool, and the dashes are the same mark the selection makes
            // on the canvas. The spans reach both corners of every side, so the
            // box still reads as a box at 18 px.
            const LO: f32 = 5.0;
            const HI: f32 = 19.0;
            for (from, to) in [(0.0, 4.0), (6.0, 8.0), (10.0, 14.0)] {
                line(at(LO + from, LO), at(LO + to, LO));
                line(at(LO + from, HI), at(LO + to, HI));
                line(at(LO, LO + from), at(LO, LO + to));
                line(at(HI, LO + from), at(HI, LO + to));
            }
        }

        Icon::Transform => {
            // A solid box with its four corner handles, which is exactly what
            // the tool draws on the canvas. Deliberately *not* dashed: the
            // dashes belong to the selection, and a transform box is a
            // different thing that happens to be the same shape.
            const LO: f32 = 6.0;
            const HI: f32 = 18.0;
            path(vec![
                at(LO, LO),
                at(HI, LO),
                at(HI, HI),
                at(LO, HI),
                at(LO, LO),
            ]);
            for (x, y) in [(LO, LO), (HI, LO), (HI, HI), (LO, HI)] {
                painter.circle_filled(at(x, y), 2.4 * scale, colour);
            }
        }

        Icon::Zoom => {
            painter.circle_stroke(at(10.0, 10.0), 5.5 * scale, stroke);
            line(at(14.2, 14.2), at(20.0, 20.0));
        }

        Icon::Rotate => {
            // Most of a circle with a head on one end, the gap at the top.
            // Deliberately not the pair of opposed arrows the same idea is
            // often drawn with: at 16 px that reads as "swap these two".
            const STEPS: usize = 20;
            const R: f32 = 7.0;
            let a0 = (-60.0_f32).to_radians();
            let a1 = 240.0_f32.to_radians();
            let mut pts = Vec::with_capacity(STEPS + 1);
            for k in 0..=STEPS {
                let a = a0 + (a1 - a0) * k as f32 / STEPS as f32;
                let (s, c) = a.sin_cos();
                pts.push(at(12.0 + c * R, 12.0 + s * R));
            }
            path(pts);
            // The head sits on the *tangent* at the open end, which is what
            // makes the ring read as travelling rather than as a circle
            // somebody left unclosed.
            let (s, c) = a1.sin_cos();
            let tip = (12.0 + c * R, 12.0 + s * R);
            let dir = (-s, c);
            for turn in [140.0_f32, -140.0] {
                let (ts, tc) = turn.to_radians().sin_cos();
                let barb = (dir.0 * tc - dir.1 * ts, dir.0 * ts + dir.1 * tc);
                line(
                    at(tip.0, tip.1),
                    at(tip.0 + barb.0 * 5.0, tip.1 + barb.1 * 5.0),
                );
            }
        }

        Icon::FlipHorizontal | Icon::FlipVertical => {
            // Two arrowheads facing away from a dashed mirror line. The dashes
            // are what say *mirror* rather than *move apart*: a solid rule
            // between two shapes reads as a divider.
            let horizontal = icon == Icon::FlipHorizontal;
            // Along the mirror line, and across it. One pair of coordinates
            // serves both icons by swapping which is which — the two marks are
            // the same drawing a quarter turn apart, and writing it twice is
            // how they end up subtly different weights.
            let put = |along: f32, across: f32| {
                if horizontal {
                    at(across, along)
                } else {
                    at(along, across)
                }
            };
            for (from, to) in [(4.0, 8.0), (10.5, 13.5), (16.0, 20.0)] {
                line(put(from, 12.0), put(to, 12.0));
            }
            for side in [-1.0_f32, 1.0] {
                path(vec![
                    put(6.5, 12.0 + side * 2.0),
                    put(17.5, 12.0 + side * 2.0),
                    put(12.0, 12.0 + side * 8.5),
                    put(6.5, 12.0 + side * 2.0),
                ]);
            }
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

        Icon::ChevronRight => path(vec![at(9.0, 6.0), at(15.0, 12.0), at(9.0, 18.0)]),

        Icon::Folder => {
            // The tab first, so the body's top edge draws over its base and the
            // two read as one shape rather than as a box with a bump.
            path(vec![
                at(3.0, 8.0),
                at(3.0, 5.5),
                at(9.5, 5.5),
                at(11.5, 8.0),
            ]);
            path(vec![
                at(3.0, 8.0),
                at(21.0, 8.0),
                at(21.0, 18.5),
                at(3.0, 18.5),
                at(3.0, 8.0),
            ]);
        }

        Icon::Eye => {
            path(eye_outline(&at));
            painter.circle_filled(at(12.0, 12.0), 2.6 * scale, colour);
        }

        Icon::EyeOff => {
            path(eye_outline(&at));
            line(at(5.0, 19.0), at(19.0, 5.0));
        }

        Icon::Mask => {
            path(vec![
                at(4.0, 5.0),
                at(20.0, 5.0),
                at(20.0, 19.0),
                at(4.0, 19.0),
                at(4.0, 5.0),
            ]);
            painter.circle_filled(at(12.0, 12.0), 4.2 * scale, colour);
        }

        Icon::Clip => {
            // The rule is the layer being clipped *to*; the arrow turns down
            // onto it. Without the rule the mark is just a return arrow.
            line(at(5.0, 19.0), at(19.0, 19.0));
            path(vec![at(16.0, 5.0), at(16.0, 14.5), at(8.0, 14.5)]);
            path(vec![at(11.5, 11.0), at(8.0, 14.5), at(11.5, 18.0)]);
        }

        Icon::Lock | Icon::Unlock => {
            // Body first, so the shackle sits on top of it at any size.
            path(vec![
                at(6.0, 11.0),
                at(18.0, 11.0),
                at(18.0, 20.0),
                at(6.0, 20.0),
                at(6.0, 11.0),
            ]);
            // Half a ring, and where its ends land is the whole difference: a
            // closed lock drops both legs onto the body, an open one lifts the
            // right-hand leg clear and shifts the arch across.
            const STEPS: usize = 12;
            let closed = icon == Icon::Lock;
            let cx = if closed { 12.0 } else { 15.0 };
            let mut pts = Vec::with_capacity(STEPS + 2);
            pts.push(at(cx - 3.6, 11.0));
            for k in 0..=STEPS {
                let a = std::f32::consts::PI + k as f32 * std::f32::consts::PI / STEPS as f32;
                let (s, c) = a.sin_cos();
                pts.push(at(cx + c * 3.6, 7.4 + s * 3.6));
            }
            if closed {
                pts.push(at(cx + 3.6, 11.0));
            }
            path(pts);
        }

        Icon::Chain => {
            // Two rounded links overlapping on the diagonal, plus the bar that
            // joins them — the bar is what stops this reading as two capsules.
            for side in [-1.0_f32, 1.0] {
                path(rotated_rect(
                    &at,
                    12.0 + side * 3.6,
                    12.0 - side * 3.6,
                    3.2,
                    5.0,
                    45.0,
                ));
            }
            line(at(9.5, 14.5), at(14.5, 9.5));
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

        Icon::Document => {
            // The turned-down corner is what separates a document from a plain
            // rectangle at 12 px, where the aspect ratio alone does not.
            path(vec![
                at(6.0, 3.5),
                at(14.0, 3.5),
                at(18.5, 8.0),
                at(18.5, 20.5),
                at(6.0, 20.5),
                at(6.0, 3.5),
            ]);
            path(vec![at(14.0, 3.5), at(14.0, 8.0), at(18.5, 8.0)]);
        }

        Icon::BrushNew => {
            // `Brush`'s stroke, shortened and moved down-left to leave the
            // top-right corner for the plus. Drawn from the same two primitives
            // so the pair read as one family at 14 px, where a second brush
            // shape would just look like a wobble.
            line(at(5.0, 19.0), at(13.0, 11.0));
            painter.circle_filled(at(5.0, 19.0), 2.6 * scale, colour);
            line(at(17.5, 3.5), at(17.5, 11.5));
            line(at(13.5, 7.5), at(21.5, 7.5));
        }

        Icon::Alert => {
            // Drawn with a flat top-left to bottom-right sweep rather than as
            // an equilateral triangle: at 16 px an equilateral one loses its
            // apex to the stroke weight and reads as a blob.
            path(vec![
                at(12.0, 3.5),
                at(22.0, 20.5),
                at(2.0, 20.5),
                at(12.0, 3.5),
            ]);
            line(at(12.0, 9.5), at(12.0, 15.0));
            painter.circle_filled(at(12.0, 18.0), 1.3 * scale, colour);
        }

        Icon::Copy => {
            // The sheet behind is drawn as three sides rather than a whole
            // rectangle: at 18 px a complete second box under the first reads
            // as one thick-walled frame, where an open corner reads as depth.
            path(vec![at(9.0, 5.0), at(19.0, 5.0), at(19.0, 15.0)]);
            path(vec![
                at(5.0, 9.0),
                at(15.0, 9.0),
                at(15.0, 19.0),
                at(5.0, 19.0),
                at(5.0, 9.0),
            ]);
        }

        Icon::Cut => {
            // Two blades crossing above two finger rings. The crossing point is
            // above centre so the rings have room to be circles rather than
            // dots — below about 14 px they merge with the blades otherwise,
            // and what is left reads as a plus.
            line(at(7.0, 4.0), at(15.5, 15.5));
            line(at(17.0, 4.0), at(8.5, 15.5));
            painter.circle_stroke(at(7.0, 18.0), 2.6 * scale, stroke);
            painter.circle_stroke(at(17.0, 18.0), 2.6 * scale, stroke);
        }

        Icon::Deselect => {
            // `Select`'s box, drawn a size smaller to leave room for the
            // stroke, with its dash spans scaled to match so the two read as
            // the same object with something done to it.
            const LO: f32 = 6.0;
            const HI: f32 = 18.0;
            for (from, to) in [(0.0, 3.5), (5.0, 7.0), (8.5, 12.0)] {
                line(at(LO + from, LO), at(LO + to, LO));
                line(at(LO + from, HI), at(LO + to, HI));
                line(at(LO, LO + from), at(LO, LO + to));
                line(at(HI, LO + from), at(HI, LO + to));
            }
            line(at(4.0, 20.0), at(20.0, 4.0));
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
