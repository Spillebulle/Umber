//! The colour picker from the design's Colour panel.
//!
//! Four of the design's five modes are implemented: a hue ring with a triangle
//! or square centre, a plain saturation/value square with a hue bar, RGB
//! sliders, and a harmony wheel. Palette is not one of these — it is a module
//! of its own, [`crate::palettelib`], because a palette is something the artist
//! *keeps* rather than a way of arriving at one colour, and it has a library
//! and a file format behind it that no picker mode wants.
//!
//! HSV is the picker's state, not the colour. Deriving hue from RGB each frame
//! would lose it whenever saturation or value hits zero — drag the value to
//! black and the hue would snap back to red on the way out. The Harmony mode
//! leans on that hardest: a harmony is a function of hue alone, so a mode that
//! read the hue off the colour would offer a red harmony for every grey.

use crate::theme::{Palette, metrics};
use egui::{
    Color32, Mesh, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2,
    epaint::Vertex, pos2, vec2,
};
use umber_core::{Harmony, Hsv};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerMode {
    Wheel,
    Square,
    Sliders,
    Harmony,
}

impl PickerMode {
    pub const ALL: [PickerMode; 4] = [Self::Wheel, Self::Square, Self::Sliders, Self::Harmony];

    pub fn label(self) -> &'static str {
        match self {
            Self::Wheel => "Wheel",
            Self::Square => "Square",
            Self::Sliders => "Sliders",
            Self::Harmony => "Harmony",
        }
    }
}

/// The shape inside the hue ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelShape {
    Triangle,
    Square,
}

impl WheelShape {
    pub const ALL: [WheelShape; 2] = [Self::Triangle, Self::Square];

    /// Where the shape sits with the Angle control at zero, in degrees.
    ///
    /// The pose every build before the angle existed drew: the triangle's apex
    /// straight up, the square's axes level. Screen y is down, so "up" is a
    /// quarter turn back from egui's zero angle, which points right.
    ///
    /// Any orientation would do for the triangle — the barycentric maths reads
    /// the three corners wherever they are — but a fixed triangle that points
    /// sideways looks like one that failed to finish turning, which is why zero
    /// is not the same number for both shapes and why the angle is stored per
    /// shape rather than shared.
    fn neutral(self) -> f32 {
        match self {
            Self::Triangle => -90.0,
            Self::Square => 0.0,
        }
    }

    /// Whether the hue is what turns this centre while "Rotate with hue" is on.
    ///
    /// The triangle alone, and it stays the triangle alone now that the square
    /// can be turned as well: the triangle has a corner that *is* the hue, so
    /// following the marker round the ring keeps the two beside each other. A
    /// square has no such corner, and turning it with the hue would only swing
    /// the saturation and value axes off the level for nothing.
    fn follows_hue(self, rotate: bool) -> bool {
        rotate && matches!(self, Self::Triangle)
    }
}

/// The angle each wheel centre is held at, in degrees from its neutral pose.
///
/// One number per shape rather than one shared between them, for two reasons.
/// The two do not have the same neutral — apex up against axes level — so a
/// single value would stand for a different pose in each and switching shapes
/// would turn whichever one was showing. And an angle is a choice *about* a
/// shape: trying the other one and coming back should find the first where it
/// was left, not where the second was.
///
/// Zero is the pose every build before this drew, which is also what a
/// preferences file written before the setting existed supplies.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WheelAngles {
    triangle: f32,
    square: f32,
}

impl WheelAngles {
    pub fn of(self, shape: WheelShape) -> f32 {
        match shape {
            WheelShape::Triangle => self.triangle,
            WheelShape::Square => self.square,
        }
    }

    /// Set one shape's angle, wrapped into a single turn.
    ///
    /// Normalising here rather than at the call sites is what lets a hand-edited
    /// preferences file, a slider drag and a default all arrive by the same
    /// door.
    pub fn set(&mut self, shape: WheelShape, degrees: f32) {
        let degrees = normalise_angle(degrees);
        match shape {
            WheelShape::Triangle => self.triangle = degrees,
            WheelShape::Square => self.square = degrees,
        }
    }
}

/// An angle in degrees, brought into `0..360`.
///
/// A whole turn is the period for both shapes. Their *outlines* repeat sooner —
/// three times round for the triangle, four for the square — but the triangle's
/// corners are a hue, a white and a black, and the square's axes are a
/// saturation and a value. A third of a turn therefore moves the colours even
/// where it leaves the outline exactly where it was, so there is no shorter
/// range to offer.
///
/// Anything not finite comes back as zero rather than being carried: this ends
/// up in `sin_cos` and then in vertex positions, and one NaN there is a mesh
/// egui discards — a picker that silently stops drawing.
///
/// The arithmetic is [`umber_core::color::wrap_hue`]'s, because it is the same
/// arithmetic and the same two traps. This name is kept because the *thing*
/// differs: a hue is a position on the colour circle and this is how far a
/// shape has been turned from its neutral pose, which is why the range is
/// documented per shape above and why the Angle control reads it in degrees of
/// rotation.
pub fn normalise_angle(degrees: f32) -> f32 {
    umber_core::color::wrap_hue(degrees)
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
///
/// This is the width of the *band*, not a distance to move a vertex by, and the
/// three shapes reach it differently: the ring pushes its vertices `f` along
/// the radius, because a circle's normal is its radius; the field pushes its
/// outer ring `f` along the field's own axes, which is exact at the corners
/// too; and the triangle needs [`SKIRT_MITRE`] times as much, for the reason
/// stated there. Moving a vertex `f` and assuming the edge followed is what
/// left the diagonals half-feathered.
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

/// How far out a press has to be to belong to the hue ring rather than to the
/// centre.
///
/// The hub's outermost 8% steers hue too, so the ring's inner edge is
/// forgiving to grab. Everything inside it — including the three wide gaps
/// between an inscribed triangle and the ring — belongs to the saturation and
/// value shape, which is where those presses have always gone.
const RING_GRIP: f32 = 0.92;

/// Draw the picker. Returns true when the colour changed.
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    p: &Palette,
    mode: PickerMode,
    shape: &mut WheelShape,
    rotate: &mut bool,
    angles: &mut WheelAngles,
    harmony: &mut Harmony,
    hsv: &mut Hsv,
) -> bool {
    match mode {
        PickerMode::Wheel => wheel(ui, p, shape, rotate, angles, hsv),
        PickerMode::Square => square(ui, p, hsv),
        PickerMode::Sliders => sliders(ui, p, hsv),
        PickerMode::Harmony => harmony_wheel(ui, p, harmony, hsv),
    }
}

/// The RGB sliders alone, for the dialogs that mix a colour without being a
/// picker.
///
/// The New document and Export dialogs each want the Colour panel's own slider
/// mode so the two mix a colour the same way, and neither has anywhere for the
/// wheel's shape, spin, angle or harmony to live — a dialog must not be able to
/// turn the picker in the panel behind it. Both used to declare four throwaway
/// locals and call [`show`], which is the same block written twice and one more
/// place to edit every time this signature grows. It is the same `sliders` the
/// mode draws, so the two cannot diverge.
pub fn show_sliders(ui: &mut Ui, p: &Palette, hsv: &mut Hsv) -> bool {
    sliders(ui, p, hsv)
}

/// The angle the wheel's centre is drawn at, in degrees from its neutral pose —
/// which is also exactly what the Angle control reads.
///
/// Following the hue *replaces* the stored angle rather than offsetting it: the
/// shape's orientation is then a function of the hue, and there is nothing left
/// for a second number to mean. That is the whole reason the Angle control is
/// drawn dead rather than live while it is on. Returning the hue's own answer in
/// the control's units is what lets the dead rail still say where the shape is.
fn wheel_angle(shape: WheelShape, rotate: bool, angles: WheelAngles, hue: f32) -> f32 {
    if shape.follows_hue(rotate) {
        normalise_angle(hue - shape.neutral())
    } else {
        angles.of(shape)
    }
}

/// Where the gesture this response is reporting began, or `None` if there is no
/// gesture.
///
/// A drag reports where the pointer is *now*, which by the end of one can be
/// anywhere; which of the wheel's two controls it belongs to has to be settled
/// from where it was pressed. egui clears the press origin on release and a
/// click is reported on the release, so a click falls back to its own position —
/// by definition it has not moved far enough for the two to differ.
fn gesture_origin(ui: &Ui, response: &Response) -> Option<Pos2> {
    if !(response.dragged() || response.clicked()) {
        return None;
    }
    ui.ctx()
        .input(|i| i.pointer.press_origin())
        .or_else(|| response.interact_pointer_pos())
}

fn wheel(
    ui: &mut Ui,
    p: &Palette,
    shape: &mut WheelShape,
    rotate: &mut bool,
    angles: &mut WheelAngles,
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

    // One interaction for the whole wheel, and where a gesture was *pressed*
    // decides which of the two controls inside it the gesture belongs to.
    //
    // Two overlapping `ui.interact` rects cannot do it: egui hands a press to
    // the topmost widget under it, and the centre's rect is the square around a
    // shape inscribed in the ring — so its corners cover the ring at the four
    // diagonals, and a hue drag begun there went to the saturation and value
    // field instead. Settling it at the press rather than per frame is also what
    // lets a drag begun on the ring carry on across the middle, which is how
    // both controls have always behaved once held.
    let response = ui.interact(area, ui.id().with("wheel"), Sense::click_and_drag());
    let at = response.interact_pointer_pos();
    let on_ring = gesture_origin(ui, &response)
        .is_some_and(|from| (from - centre).length() > inner * RING_GRIP);

    // --- hue ring ---
    if on_ring && let Some(pos) = at {
        hsv.h = ring_hue(centre, pos);
        changed = true;
    }

    hue_ring(ui, centre, inner, outer);

    // Hue marker, on the middle of the ring wherever the ring ended up.
    ui.painter()
        .circle_stroke(ring_point(centre, inner, outer, hsv.h), 6.0, MARKER_STROKE);

    // --- saturation / value shape ---
    let drag = if on_ring { None } else { at };
    let base = (shape.neutral() + wheel_angle(*shape, *rotate, *angles, hsv.h)).to_radians();
    match shape {
        WheelShape::Square => {
            // Largest square that fits inside the ring. Inscribed in the circle,
            // so turning it cannot take it outside — the size is the same
            // whatever the angle.
            let half = (inner * std::f32::consts::FRAC_1_SQRT_2 - 2.0).max(1.0);
            changed |= sv_field(ui, centre, Vec2::splat(half), base, drag, hsv);
        }
        WheelShape::Triangle => {
            changed |= sv_triangle(ui, centre, (inner - 3.0).max(1.0), base, drag, hsv);
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

    // Only the triangle follows the hue — see `WheelShape::follows_hue` — so the
    // row is drawn only when the triangle is showing, rather than drawn
    // disabled: for the square the setting has no meaning at all, and a dead
    // control here would be asking which of the two shapes it refers to.
    if *shape == WheelShape::Triangle {
        crate::widgets::toggle_row(ui, p, "Rotate with hue", rotate);
    }

    // The angle the shape is held at, when the hue is not deciding it.
    //
    // A rail with a typeable figure — [`crate::widgets::number_row`] — rather
    // than a plain one. Two of the angles anybody actually wants here are
    // exact: the neutral pose, and a quarter turn from it. Landing on either by
    // dragging a rail 240 px wide across 360° is a matter of luck, so the rail
    // snaps to each 45° and the figure beside it can be typed. Forty-five
    // rather than ninety because a triangle's own symmetry is a third of a turn
    // and a square's a quarter: 45° is the coarsest step that has every
    // half-way pose of both shapes on it.
    //
    // A slider rather than a drag on the shape itself. Both regions of the wheel
    // are already spoken for: the ring steers hue, and the centre is the
    // saturation and value field right out to the gaps between an inscribed
    // shape and the ring, where `clamp_barycentric` deliberately slides along an
    // edge rather than freezing. A rotate gesture could therefore only be a
    // modifier or a third zone carved out of one of those, and neither is
    // something anyone would find without being told.
    //
    // Drawn disabled rather than hidden while the hue *is* deciding it. The
    // setting still holds a value and the shape still has an angle — it is only
    // that something else is supplying it — so the rail goes on reading where
    // the shape actually is, and the row does not appear and disappear as the
    // toggle above it is flipped. Hiding is what that toggle does for the
    // square, and the difference is that there the setting means nothing at all,
    // where here it means something that is temporarily spoken for.
    //
    // Recomputed rather than reusing `base`: the switch above may have changed
    // shape this very frame, and the angle belongs to the shape.
    let following = shape.follows_hue(*rotate);
    let mut degrees = wheel_angle(*shape, *rotate, *angles, hsv.h);
    let row = ui.scope(|ui| {
        if following {
            ui.disable();
        }
        crate::widgets::number_row(ui, p, &mut degrees, angle_row())
    });
    if row.inner {
        angles.set(*shape, degrees);
    }
    if following {
        row.response
            .on_hover_text("The hue is setting the angle — turn Rotate with hue off to set it.");
    }

    changed
}

/// The Angle control's shape: a whole turn, a figure in degrees, landing on
/// each 45°.
///
/// Split out so the tests drive the control the picker actually draws rather
/// than a copy of its numbers — the same reason [`triangle_corners`] is its own
/// function.
fn angle_row() -> crate::widgets::NumberRow<'static> {
    crate::widgets::NumberRow {
        label: "Angle",
        range: 0.0..=359.0,
        snap: 45.0,
        per_unit: 1.0,
        suffix: "°",
        decimals: 0,
        // The wheel is not drawn inside the thing this turns, so there is
        // nothing to run away from the pointer as it is dragged.
        deferred: false,
    }
}

/// The white ring every marker in this picker is drawn with.
///
/// One value rather than a number typed at each of the four call sites: the
/// hue marker, the triangle's, the field's and the harmony's members are the
/// same instrument saying "you are here", and a marker that was two points on
/// one control and one on another would read as two different things.
const MARKER_STROKE: Stroke = Stroke {
    width: 2.0,
    color: Color32::WHITE,
};

/// The middle of the ring at a given hue — where a marker for that hue goes.
fn ring_point(centre: Pos2, inner: f32, outer: f32, hue: f32) -> Pos2 {
    let a = hue.to_radians();
    centre + vec2(a.cos(), a.sin()) * (inner + outer) * 0.5
}

/// The hue a point on the ring stands for — [`ring_point`]'s inverse, and the
/// only thing in this file that writes a hue from a gesture.
///
/// Through [`umber_core::color::wrap_hue`] rather than a bare `rem_euclid`,
/// which is what both call sites used to do. `atan2` answers in `-π..=π`, and
/// `(-1e-7f32).to_degrees().rem_euclid(360.0)` is exactly `360.0` — a hue one
/// step anticlockwise of red, held in the one value the type says it never
/// holds, and `to_color` reads it as the sixth sextant and paints magenta.
fn ring_hue(centre: Pos2, at: Pos2) -> f32 {
    let d = at - centre;
    umber_core::color::wrap_hue(d.y.atan2(d.x).to_degrees())
}

/// The hue ring itself, drawn between two radii about `centre`.
///
/// Its own function because the Wheel and Harmony modes both draw one and a
/// second copy of a hand-built mesh is a second thing to keep feathered. See
/// [`feather`] for why the mesh carries its own skirt at all.
fn hue_ring(ui: &Ui, centre: Pos2, inner: f32, outer: f32) {
    let f = feather(ui);
    // Enough segments that the facets are shorter than the feather is wide,
    // otherwise a smooth edge is drawn along a visibly polygonal outline. Scaled
    // to the wheel rather than fixed: a picker in a narrow panel does not need
    // the segments a wide one does, and a wide one needed more than the flat 96
    // this used to draw.
    let segments = ((outer * 0.9) as usize).clamp(RING_SEGMENTS, 320);

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
    ui.painter().add(Shape::mesh(mesh));
}

/// Saturation/value triangle inscribed in the ring, turned by `base` radians.
///
/// The apex is the full hue, with white and black at the other two corners.
/// `base` is where that apex points: the hue when the shape is following it,
/// which is what the design shows, and otherwise the shape's neutral pose
/// turned by the user's own angle.
///
/// The choice is not cosmetic. Following the hue keeps the apex next to the
/// marker that sets it, so the two controls read as one instrument — but it
/// also means the whole saturation/value field swings under the pointer while
/// the ring is being dragged, so a tint chosen at one hue is somewhere else at
/// the next. Holding still gives up the first to get the second: the point you
/// last picked stays where you left it, and picking the same tint across
/// several hues becomes a matter of returning to the same place.
///
/// `drag` is where the pointer is, if this frame's gesture belongs to the
/// centre. The wheel decides that — see the interaction comment there — so
/// there is no `interact` of its own to overlap the ring's.
fn sv_triangle(
    ui: &mut Ui,
    centre: Pos2,
    radius: f32,
    base: f32,
    drag: Option<Pos2>,
    hsv: &mut Hsv,
) -> bool {
    let mut changed = false;
    let (hue_pt, white_pt, black_pt) = triangle_corners(centre, radius, base);

    if let Some(pos) = drag {
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
    // The centroid gives each corner its outward direction. For an equilateral
    // triangle that direction is also the corner's bisector, which is what
    // [`skirt_corner`] needs — and how far along it to go is the whole of why
    // the diagonals used to stair-step while the ring did not.
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
        mesh.vertices.push(Vertex {
            pos: skirt_corner(pt, centroid, f),
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
    painter.circle_stroke(marker, 5.0, MARKER_STROKE);

    changed
}

/// The triangle's three corners, in the order the rest of this file reads them:
/// full hue, white, black.
///
/// Split out from [`sv_triangle`] because it is the whole of what the angle
/// changes, and everything else about the triangle — the hit test, the mesh,
/// the marker — is derived from these three points. Testing it without a `Ui`
/// is therefore testing the feature.
fn triangle_corners(centre: Pos2, radius: f32, base: f32) -> (Pos2, Pos2, Pos2) {
    let corner = |k: f32| {
        let a = base + k * std::f32::consts::TAU / 3.0;
        centre + vec2(a.cos(), a.sin()) * radius
    };
    (corner(0.0), corner(1.0), corner(2.0))
}

/// How much further out a corner of the triangle goes than its edges do.
///
/// The ring's skirt is easy, and that is exactly why the triangle's looked like
/// it had one when it did not. A circle's outward *normal* is its radial
/// direction, so a vertex pushed a feather out from the centre carries its edge
/// a feather out with it and the faded band is one pixel wide the whole way
/// round. A triangle's corners are not its edges. Pushing a corner out along
/// the line from the centroid scales the whole shape about that point, and an
/// equilateral triangle's inradius is *half* its circumradius — so a corner
/// moved by `f` moves each of the three edges by only `f/2`. The diagonals were
/// being feathered over half a pixel while the ring got a whole one, which is
/// the whole of why they went on stair-stepping.
///
/// What an edge needs is to be offset `f` along its own normal, and the corner
/// then goes where two such offset edges meet: `f / sin(θ/2)` along the
/// bisector, for an interior angle θ. [`triangle_corners`] places its three
/// points a third of a turn apart on a circle, so this triangle is *always*
/// equilateral — θ is always 60°, `sin(30°)` is exactly a half, and the factor
/// is exactly 2. Nothing here has to measure an angle, and there is no case
/// where it is some other number.
///
/// Widening the skirt is also the only change worth making: turning the
/// interior into more triangles would not help, because the aliasing is at the
/// boundary and the gradient across the middle is already exact at three
/// vertices.
const SKIRT_MITRE: f32 = 2.0;

/// One corner of the skirt: the triangle's corner pushed out along its bisector
/// far enough that all three edges move out by exactly `f`.
///
/// For an equilateral triangle the direction from the centroid to a corner *is*
/// that corner's bisector, so the centroid is all this needs to be told.
///
/// A triangle squashed to nothing has no outward direction, and normalising it
/// would put NaN into the mesh — which egui discards whole, so the picker would
/// simply stop drawing.
fn skirt_corner(pt: Pos2, centroid: Pos2, f: f32) -> Pos2 {
    let away = pt - centroid;
    if away.length() > 1e-3 {
        pt + away.normalized() * f * SKIRT_MITRE
    } else {
        pt
    }
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

    let (rect, response) = ui.allocate_exact_size(vec2(width, 130.0), Sense::click_and_drag());
    // Nothing overlaps this one, so it does its own interacting — unlike the
    // wheel's centre, which shares a hit area with the ring.
    let drag = (response.dragged() || response.clicked())
        .then(|| response.interact_pointer_pos())
        .flatten();
    // Square on the page and level: this mode has no ring to turn inside, so
    // the angle a wheel centre carries is not a setting here.
    changed |= sv_field(ui, rect.center(), rect.size() * 0.5, 0.0, drag, hsv);

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

/// The field's own axes: across, rising saturation, and down, falling value.
///
/// At an angle of zero these are exactly `(1, 0)` and `(0, 1)`, so a level field
/// puts every vertex where the axis-aligned version put it. That exactness is
/// what lets the plain square mode and the wheel's turnable centre be one
/// implementation rather than two that have to agree.
fn field_axes(angle: f32) -> (Vec2, Vec2) {
    let (sin, cos) = angle.sin_cos();
    (vec2(cos, sin), vec2(-sin, cos))
}

/// Where a saturation and value land in the field.
fn field_point(centre: Pos2, half: Vec2, angle: f32, s: f32, v: f32) -> Pos2 {
    let (across, down) = field_axes(angle);
    centre + across * (half.x * (s * 2.0 - 1.0)) + down * (half.y * (1.0 - v * 2.0))
}

/// The reverse: what saturation and value a point stands for.
///
/// Clamped to the field rather than rejected, for the reason
/// [`clamp_barycentric`] gives — dragging past an edge slides along it instead
/// of freezing the picker. A field squashed to nothing would divide by zero, and
/// one NaN reaching a vertex is a mesh egui discards.
fn field_at(centre: Pos2, half: Vec2, angle: f32, pos: Pos2) -> (f32, f32) {
    let (across, down) = field_axes(angle);
    let d = pos - centre;
    let u = (d.dot(across) / half.x.max(1e-3)).clamp(-1.0, 1.0);
    let w = (d.dot(down) / half.y.max(1e-3)).clamp(-1.0, 1.0);
    ((u + 1.0) * 0.5, (1.0 - w) * 0.5)
}

/// One axis of the field's mesh grid: the parameter in `-1..=1`, and how far out
/// of the field this row or column sits. The first and last are the skirt.
fn field_edge(i: usize, n: usize) -> (f32, f32) {
    if i == 0 {
        (-1.0, -1.0)
    } else if i == n + 2 {
        (1.0, 1.0)
    } else {
        ((i - 1) as f32 / n as f32 * 2.0 - 1.0, 0.0)
    }
}

/// The saturation/value gradient: white→hue across, black down, turned by
/// `angle` radians about `centre`.
///
/// `half` is the semi-axes before the turn, so the plain square mode passes a
/// rectangle's and the wheel passes a square's. `drag` is where the pointer is,
/// if this frame's gesture belongs to the field.
fn sv_field(
    ui: &mut Ui,
    centre: Pos2,
    half: Vec2,
    angle: f32,
    drag: Option<Pos2>,
    hsv: &mut Hsv,
) -> bool {
    let mut changed = false;

    if let Some(pos) = drag {
        (hsv.s, hsv.v) = field_at(centre, half, angle, pos);
        changed = true;
    }

    // Four-corner interpolation is not enough — saturation and value do not
    // multiply linearly — so the field is drawn as a small grid.
    const N: usize = 8;
    // One ring of vertices outside it, pushed a feather out along the field's
    // own axes and faded to nothing. A turned field has four diagonal edges, and
    // a mesh handed to egui whole never goes through its tessellator's own
    // antialiasing — see `feather`. Pushing along the axes rather than away from
    // the centre makes the skirt exactly one feather wide the whole way round,
    // corners included.
    let f = feather(ui);
    let (across, down) = field_axes(angle);
    let steps = N + 2;

    let painter = ui.painter();
    let mut mesh = Mesh::default();
    for iy in 0..=steps {
        for ix in 0..=steps {
            let (u, out_u) = field_edge(ix, N);
            let (w, out_w) = field_edge(iy, N);
            let (s, v) = ((u + 1.0) * 0.5, (1.0 - w) * 0.5);
            let colour = hsv_colour(hsv.h, s, v);
            mesh.vertices.push(Vertex {
                pos: centre + across * (half.x * u + f * out_u) + down * (half.y * w + f * out_w),
                uv: egui::epaint::WHITE_UV,
                color: if out_u == 0.0 && out_w == 0.0 {
                    colour
                } else {
                    faded(colour)
                },
            });
        }
    }
    let stride = (steps + 1) as u32;
    for iy in 0..steps as u32 {
        for ix in 0..steps as u32 {
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

    let marker = field_point(centre, half, angle, hsv.s, hsv.v);
    painter.circle_stroke(marker, 5.5, MARKER_STROKE);

    changed
}

/// How tall a harmony swatch is, and how far apart two of them sit.
const HARMONY_SWATCH: f32 = 28.0;
const HARMONY_GAP: f32 = 4.0;

/// The bar under the swatch that is the colour in hand, and the gap above it.
///
/// **Under** the swatch rather than round it, and this is not a preference. An
/// accent border on a swatch is invisible whenever the colour is near the
/// accent — which for Umber's own accent is any warm ochre, and a warm ochre is
/// exactly what somebody reaches a harmony wheel for. A ring in white or black
/// picked by luminance would always contrast and would be a mark that changes
/// shape as the colour moves; a bar on the panel's own background always reads,
/// says the same thing every time, and is in the accent this interface already
/// spells "this is the one" in.
const HARMONY_MARK: f32 = 3.0;
const HARMONY_MARK_GAP: f32 = 3.0;

/// The whole height one member of the row takes: the colour, and room for the
/// mark under it whether or not this one wears it.
///
/// Reserved for every member rather than added to the one that has it, or the
/// row would be three pixels taller the moment the base moved and every swatch
/// would step down under the pointer.
const HARMONY_ROW: f32 = HARMONY_SWATCH + HARMONY_MARK_GAP + HARMONY_MARK;

/// The hue ring with the chosen relation's other hues marked on it, a
/// saturation/value square in the middle, and a row of the resulting colours
/// underneath.
///
/// Three controls, and each does exactly one thing:
///
/// * **The ring** sets the hue in hand, by dragging, exactly as the Wheel
///   mode's does. Every marker on it moves together, because a harmony is a
///   rotation.
/// * **The square** sets saturation and value. Without it this mode could not
///   set them at all, and a picker mode you have to leave to finish choosing a
///   colour is one nobody would stay in.
/// * **The swatch row** takes one of the related colours, by clicking. That is
///   the precise control: a click on the ring near a marker would land on
///   *roughly* that hue, where the whole point of a harmony is the exact angle.
///
/// Saturation and value are the artist's own and are carried across every
/// member — see [`umber_core::harmony`], which is where the argument for that
/// lives. It also means a harmony of a grey is a row of identical greys, which
/// is correct and is why the base swatch is marked rather than left to be told
/// apart by its colour.
fn harmony_wheel(ui: &mut Ui, p: &Palette, harmony: &mut Harmony, hsv: &mut Hsv) -> bool {
    let mut changed = false;

    let size = ui.available_width().clamp(MIN_PICKER, 176.0);
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width().max(size), size), Sense::hover());
    let area = Rect::from_center_size(rect.center(), vec2(size, size));
    let centre = area.center();
    let outer = size * 0.5;
    let inner = (outer - RING_THICKNESS).max(outer * 0.25);

    // One interaction for the whole wheel, settled at the press, for exactly
    // the reason the Wheel mode's is: the centre's rect covers the ring at the
    // four diagonals, so two overlapping `interact` rects would send a hue drag
    // begun there to the saturation and value field.
    let response = ui.interact(area, ui.id().with("harmony-wheel"), Sense::click_and_drag());
    let at = response.interact_pointer_pos();
    let on_ring = gesture_origin(ui, &response)
        .is_some_and(|from| (from - centre).length() > inner * RING_GRIP);

    if on_ring && let Some(pos) = at {
        hsv.h = ring_hue(centre, pos);
        changed = true;
    }

    hue_ring(ui, centre, inner, outer);

    // The saturation and value field, level and inscribed in the ring.
    let half = (inner * std::f32::consts::FRAC_1_SQRT_2 - 2.0).max(1.0);
    let drag = if on_ring { None } else { at };
    changed |= sv_field(ui, centre, Vec2::splat(half), 0.0, drag, hsv);

    ui.add_space(8.0);

    // Which relation. A dropdown rather than a segmented control: five names,
    // the longest of them "Split complementary", in a panel 264 px wide.
    let mut picked = *harmony;
    crate::widgets::dropdown(
        ui,
        p,
        crate::widgets::Dropdown::new(harmony.label()).width(crate::widgets::DropdownWidth::Fill),
        |ui| {
            for option in Harmony::ALL {
                if ui
                    .selectable_label(*harmony == option, option.label())
                    .clicked()
                {
                    picked = option;
                }
            }
        },
    );
    *harmony = picked;

    // Only now: the hue, the saturation, the value and the relation are all
    // settled for this frame. Asking earlier — which is the obvious place,
    // beside the ring the markers sit on — makes the row show the *previous*
    // relation for a frame after the dropdown is used, which is one frame of a
    // control lying about what it just did.
    ui.add_space(8.0);
    changed |= harmony_swatches(ui, p, harmony.hues(hsv.h).as_slice(), hsv);

    // The markers, last of all, so a colour taken from the row above moves them
    // on the frame it was taken rather than the frame after. Painting order is
    // insertion order within a layer, so these land on top of the ring however
    // far down the layout they are written.
    //
    // The base wears the same ring the Wheel mode's hue marker does — it is the
    // same thing — and the others are filled discs of their own colour, so which
    // one is in hand is a difference of *mark* rather than of colour. That
    // matters: at zero saturation every member is the same grey.
    let painter = ui.painter();
    for (index, hue) in harmony.hues(hsv.h).as_slice().iter().enumerate() {
        let at = ring_point(centre, inner, outer, *hue);
        if index == 0 {
            painter.circle_stroke(at, 6.0, MARKER_STROKE);
        } else {
            painter.circle_filled(at, 5.0, hsv_colour(*hue, hsv.s, hsv.v));
            painter.circle_stroke(at, 5.0, Stroke::new(1.5, Color32::WHITE));
        }
    }

    changed
}

/// Where one swatch of a row of `count` sits.
///
/// Its own function so the arithmetic can be checked without a `Ui`: it is the
/// one part of the row that can be wrong in a way nobody notices — a cell that
/// overlapped its neighbour would put two hit targets on the same pixels, and
/// the one drawn second would take every click on the overlap.
///
/// A width of zero is a panel dragged to nothing, and a `Rect` built from a
/// negative width has its max left of its min, which paints somewhere
/// unrelated. Hence the floor.
fn swatch_cell(row: Rect, index: usize, count: usize) -> Rect {
    let count = count.max(1);
    let each = ((row.width() - HARMONY_GAP * (count - 1) as f32) / count as f32).max(1.0);
    Rect::from_min_size(
        pos2(row.left() + index as f32 * (each + HARMONY_GAP), row.top()),
        vec2(
            each,
            (row.height() - HARMONY_MARK_GAP - HARMONY_MARK).max(1.0),
        ),
    )
}

/// The bar under one swatch, drawn only for the colour in hand.
fn swatch_mark(cell: Rect) -> Rect {
    Rect::from_min_size(
        pos2(cell.left(), cell.bottom() + HARMONY_MARK_GAP),
        vec2(cell.width(), HARMONY_MARK),
    )
}

/// The row of colours a harmony reaches, the one in hand first. Returns true
/// when one of them was taken.
///
/// Laid out by hand rather than in a `horizontal`, because the row has to
/// divide the panel's width between however many members the relation has —
/// two for a complementary, four for a tetrad — and share the remainder rather
/// than leaving a ragged end.
fn harmony_swatches(ui: &mut Ui, p: &Palette, hues: &[f32], hsv: &mut Hsv) -> bool {
    if hues.is_empty() {
        return false;
    }
    let width = ui.available_width().max(MIN_PICKER);
    let (row, _) = ui.allocate_exact_size(vec2(width, HARMONY_ROW), Sense::hover());

    let mut taken = None;
    for (index, hue) in hues.iter().enumerate() {
        let cell = swatch_cell(row, index, hues.len());
        let colour = Hsv::new(*hue, hsv.s, hsv.v).to_color(1.0);
        let [r, g, b, _] = colour.to_srgb_u8();
        // The one in hand senses hover and not clicks. It is already the colour
        // you have, so there is nothing for a click to do — and a target that
        // looks live and quietly drops what it is given is the control this
        // project refuses everywhere else. The hover stays, because the tooltip
        // saying which swatch this *is* is the whole reason to draw it.
        let response = ui.interact(
            cell,
            ui.id().with(("harmony-swatch", index)),
            if index == 0 {
                Sense::hover()
            } else {
                Sense::click()
            },
        );
        ui.painter()
            .rect_filled(cell, metrics::RADIUS, Color32::from_rgb(r, g, b));
        // An edge on every swatch, or one the colour of the panel has none.
        ui.painter().rect_stroke(
            cell,
            metrics::RADIUS,
            Stroke::new(1.0, p.border),
            StrokeKind::Inside,
        );
        // And the bar under the one in hand. See `HARMONY_MARK` for why it is
        // under rather than round.
        let hint = if index == 0 {
            ui.painter()
                .rect_filled(swatch_mark(cell), metrics::RADIUS, p.accent);
            "The colour in hand"
        } else {
            "Take this colour"
        };
        if response
            .on_hover_text(format!("{hint} — #{r:02X}{g:02X}{b:02X}"))
            .clicked()
        {
            taken = Some(*hue);
        }
    }

    // Applied after the loop rather than inside it: moving the hue mid-row
    // would redraw the remaining swatches against a base that had already
    // changed, so the second half of the row would be a different harmony from
    // the first.
    if let Some(hue) = taken {
        hsv.h = hue;
        return true;
    }
    false
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

    /// The triangle as the wheel builds it: a shape, the rotate flag, the stored
    /// angles and a hue, resolved through the one function that decides.
    fn corners_at(
        shape: WheelShape,
        rotate: bool,
        angles: WheelAngles,
        hue: f32,
    ) -> (Pos2, Pos2, Pos2) {
        let base = (shape.neutral() + wheel_angle(shape, rotate, angles, hue)).to_radians();
        triangle_corners(CENTRE, RADIUS, base)
    }

    fn corners(rotate: bool, hue: f32) -> (Pos2, Pos2, Pos2) {
        corners_at(WheelShape::Triangle, rotate, WheelAngles::default(), hue)
    }

    fn turned(degrees: f32) -> (Pos2, Pos2, Pos2) {
        let mut angles = WheelAngles::default();
        angles.set(WheelShape::Triangle, degrees);
        corners_at(WheelShape::Triangle, false, angles, 0.0)
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

    /// The angle is measured from each shape's own neutral pose, so zero is
    /// exactly what every build before it existed drew — and an older
    /// preferences file, which supplies zero by saying nothing, changes nothing.
    #[test]
    fn an_angle_of_zero_is_the_pose_the_picker_has_always_had() {
        assert_eq!(turned(0.0), corners(false, 0.0));
        assert_eq!(WheelAngles::default().of(WheelShape::Triangle), 0.0);
        assert_eq!(WheelAngles::default().of(WheelShape::Square), 0.0);
        // The square's neutral is level, not apex-up: the two shapes do not
        // share a zero, which is half of why they do not share an angle.
        let (across, down) = field_axes(WheelShape::Square.neutral().to_radians());
        assert_eq!(across, vec2(1.0, 0.0));
        assert_eq!(down, vec2(0.0, 1.0));
    }

    /// A quarter turn puts the apex where egui's zero angle points — to the
    /// right — because the neutral is a quarter turn back from it.
    #[test]
    fn the_angle_turns_the_triangle_by_exactly_that_much() {
        let (hue_pt, ..) = turned(90.0);
        assert!((hue_pt.x - (CENTRE.x + RADIUS)).abs() < 1e-3, "{hue_pt:?}");
        assert!((hue_pt.y - CENTRE.y).abs() < 1e-3, "{hue_pt:?}");
    }

    /// Why the range is a whole turn and not a third of one. A third of a turn
    /// maps the triangle's *outline* onto itself — but the corners are a hue, a
    /// white and a black, so what it actually does is hand each corner the next
    /// one's colour. The shape looks unmoved and the picker is completely
    /// different, which is exactly the case a shortened range would hide.
    #[test]
    fn a_third_of_a_turn_moves_the_triangles_colours_and_not_its_outline() {
        let (hue_pt, white_pt, black_pt) = turned(0.0);
        let (hue_now, white_now, black_now) = turned(120.0);
        // The outline is the same three points, in a different order.
        assert!(apart(hue_now, white_pt) < 1e-3, "{hue_now:?}");
        assert!(apart(white_now, black_pt) < 1e-3, "{white_now:?}");
        assert!(apart(black_now, hue_pt) < 1e-3, "{black_now:?}");
        // And the hue corner has genuinely moved, which is what the picker
        // shows and the outline does not.
        assert!(apart(hue_now, hue_pt) > RADIUS);
    }

    /// The angle is meaningless while the hue is supplying it, so the picker
    /// must not quietly add the two together — that is what the Angle control
    /// being drawn dead promises, and this is that promise in the model.
    #[test]
    fn a_triangle_following_the_hue_ignores_the_stored_angle() {
        let mut angles = WheelAngles::default();
        for degrees in [0.0, 17.0, 120.0, 359.0] {
            angles.set(WheelShape::Triangle, degrees);
            for hue in [0.0, 47.0, 210.0] {
                assert_eq!(
                    corners_at(WheelShape::Triangle, true, angles, hue),
                    corners(true, hue),
                    "angle {degrees} leaked in at hue {hue}"
                );
            }
        }
    }

    /// "Rotate with hue" is the triangle's alone — a square has no corner that
    /// is the hue — so a square is turned by its own angle whatever the flag
    /// says.
    #[test]
    fn a_square_is_turned_only_by_its_own_angle() {
        let mut angles = WheelAngles::default();
        angles.set(WheelShape::Square, 30.0);
        for rotate in [true, false] {
            assert_eq!(wheel_angle(WheelShape::Square, rotate, angles, 200.0), 30.0);
        }
        assert!(!WheelShape::Square.follows_hue(true));
        assert!(WheelShape::Triangle.follows_hue(true));
    }

    /// Each shape keeps its own number: trying the other one and coming back
    /// finds the first where it was left.
    #[test]
    fn each_shape_remembers_its_own_angle() {
        let mut angles = WheelAngles::default();
        angles.set(WheelShape::Triangle, 45.0);
        assert_eq!(
            angles.of(WheelShape::Square),
            0.0,
            "the square did not move"
        );
        angles.set(WheelShape::Square, 200.0);
        assert_eq!(
            angles.of(WheelShape::Triangle),
            45.0,
            "nor the triangle back"
        );
    }

    /// The ring's own inverse, and the trap it exists for: a point a hair
    /// anticlockwise of red is `-1e-7` radians, whose degrees `rem_euclid`
    /// rounds up to exactly 360 — outside the range `Hsv` promises, and read by
    /// `to_color` as the sixth sextant, which is magenta.
    #[test]
    fn the_ring_reads_back_the_hue_its_marker_was_drawn_at() {
        for hue in [0.0_f32, 1.0, 90.0, 179.9, 270.0, 359.9] {
            let at = ring_point(CENTRE, 40.0, 60.0, hue);
            let back = ring_hue(CENTRE, at);
            let apart = (back - hue).abs().min(360.0 - (back - hue).abs());
            assert!(apart < 1e-2, "{hue} -> {back}");
        }
        // Every answer is a hue, including the one just short of a whole turn.
        let just_under = ring_hue(CENTRE, CENTRE + vec2(1.0, -1e-7));
        assert!((0.0..360.0).contains(&just_under), "{just_under}");
        assert_eq!(
            Hsv::new(just_under, 1.0, 1.0).to_color(1.0).to_srgb_u8(),
            [255, 0, 0, 255],
            "a hair off red must not be magenta"
        );
        // And the centre itself, where there is no direction at all.
        assert!(ring_hue(CENTRE, CENTRE).is_finite());
    }

    #[test]
    fn an_angle_wraps_into_a_single_turn() {
        assert_eq!(normalise_angle(0.0), 0.0);
        assert_eq!(normalise_angle(359.0), 359.0);
        assert_eq!(normalise_angle(360.0), 0.0);
        assert_eq!(normalise_angle(370.0), 10.0);
        assert_eq!(normalise_angle(-90.0), 270.0);
        assert_eq!(normalise_angle(-720.0), 0.0);
        // A tiny negative is where `rem_euclid` rounds up to exactly a turn,
        // which is the one value outside the range it promises.
        assert!(normalise_angle(-1e-7) < 360.0);
        // Nothing non-finite may reach `sin_cos`: one NaN vertex is a mesh egui
        // discards, which reads as a picker that has stopped drawing.
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(normalise_angle(bad), 0.0);
        }
        let mut angles = WheelAngles::default();
        angles.set(WheelShape::Triangle, f32::NAN);
        assert_eq!(angles.of(WheelShape::Triangle), 0.0, "set normalises too");
    }

    /// Whatever the field is turned to, a point still stands for the same
    /// saturation and value it did — the gradient turns with the field rather
    /// than sliding under it.
    #[test]
    fn a_turned_field_reads_the_same_colour_at_the_same_place() {
        let half = vec2(40.0, 40.0);
        for angle in [0.0, 0.4, 1.0, -2.5, std::f32::consts::TAU] {
            for (s, v) in [
                (0.0, 0.0),
                (1.0, 0.0),
                (0.0, 1.0),
                (1.0, 1.0),
                (0.5, 0.5),
                (0.25, 0.8),
            ] {
                let at = field_point(CENTRE, half, angle, s, v);
                let (back_s, back_v) = field_at(CENTRE, half, angle, at);
                assert!((back_s - s).abs() < 1e-4, "{s} -> {back_s} at {angle}");
                assert!((back_v - v).abs() < 1e-4, "{v} -> {back_v} at {angle}");
            }
        }
    }

    /// A level field has to land exactly where the axis-aligned version put it,
    /// because the plain square mode and the wheel's turnable centre are now one
    /// implementation. Anything less than exact would be a mode that shifted by
    /// a fraction of a pixel for no reason.
    #[test]
    fn a_level_field_is_the_axis_aligned_one() {
        let rect = Rect::from_min_size(pos2(10.0, 20.0), vec2(200.0, 130.0));
        let (centre, half) = (rect.center(), rect.size() * 0.5);
        // The corners exactly: those are the four the eye can check against the
        // rect, and the arithmetic that reaches them is exact.
        for (s, v) in [(0.0, 1.0), (1.0, 1.0), (0.0, 0.0), (1.0, 0.0)] {
            let at = field_point(centre, half, 0.0, s, v);
            assert_eq!(
                at,
                pos2(
                    rect.left() + rect.width() * s,
                    rect.top() + rect.height() * (1.0 - v)
                )
            );
        }
        // And the inside to within a rounding step, which is all a different
        // order of the same multiplications can promise.
        for (s, v) in [(0.37, 0.62), (0.5, 0.5), (0.9, 0.1)] {
            let at = field_point(centre, half, 0.0, s, v);
            assert!(
                (at.x - (rect.left() + rect.width() * s)).abs() < 1e-3,
                "{at:?}"
            );
            assert!(
                (at.y - (rect.top() + rect.height() * (1.0 - v))).abs() < 1e-3,
                "{at:?}"
            );
        }
    }

    /// The field's edges are drawn with a skirt, and the skirt has to be outside
    /// it — a row folded back inside would paint a faded band across the
    /// gradient.
    #[test]
    fn the_fields_skirt_lies_outside_it() {
        const N: usize = 8;
        assert_eq!(field_edge(0, N), (-1.0, -1.0));
        assert_eq!(field_edge(N + 2, N), (1.0, 1.0));
        assert_eq!(field_edge(1, N), (-1.0, 0.0));
        assert_eq!(field_edge(N + 1, N), (1.0, 0.0));
        // The interior is evenly spaced and covers the whole field.
        for i in 1..=N + 1 {
            let (u, out) = field_edge(i, N);
            assert_eq!(out, 0.0);
            assert!((-1.0..=1.0).contains(&u), "{u}");
        }
    }

    /// The Angle control's figure has to survive being read and typed back:
    /// somebody who wants exactly 90° types it, and what they get has to be
    /// what the readout then says. `normalise_angle` is the door every angle
    /// comes through, so the round trip is tested with it in place.
    #[test]
    fn a_typed_angle_comes_back_as_the_angle_it_reads() {
        let row = angle_row();
        for degrees in [0.0, 45.0, 90.0, 137.0, 315.0, 359.0] {
            let typed = row
                .parse(&row.format(degrees))
                .expect("the readout is a figure the field accepts");
            assert_eq!(normalise_angle(typed), degrees);
            let mut angles = WheelAngles::default();
            angles.set(WheelShape::Triangle, typed);
            assert_eq!(angles.of(WheelShape::Triangle), degrees);
        }
    }

    /// The angle typed straight into the picker's own geometry: 90° has to be
    /// the exact quarter turn, not one rounded through a rail's pixel width.
    #[test]
    fn a_typed_quarter_turn_is_exactly_a_quarter_turn() {
        let row = angle_row();
        let typed = row.parse("90").expect("a bare figure is accepted");
        assert_eq!(turned(typed), turned(90.0));
    }

    /// The skirt has to be one feather wide along the **edges**, which is where
    /// the stair-steps are. Pushing a vertex out by a feather — what the ring
    /// does, correctly, because its normal is radial — moves an equilateral
    /// triangle's edges by only half of one, and half a pixel of fade is what
    /// left the diagonals visibly stepped.
    #[test]
    fn the_triangles_skirt_is_one_feather_wide_along_every_edge() {
        let f = 1.0;
        for degrees in [0.0_f32, 17.0, 45.0, 120.0, 271.0, 359.0] {
            let (a, b, c) = triangle_corners(CENTRE, RADIUS, degrees.to_radians());
            let inner = [a, b, c];
            let centroid = pos2(
                inner.iter().map(|p| p.x).sum::<f32>() / 3.0,
                inner.iter().map(|p| p.y).sum::<f32>() / 3.0,
            );
            let outer = inner.map(|pt| skirt_corner(pt, centroid, f));

            for (i, j) in [(0, 1), (1, 2), (2, 0)] {
                let dir = inner[j] - inner[i];
                // Perpendicular distance from each end of the outer edge to the
                // line the inner edge lies on. Both, so the two are parallel as
                // well as far enough apart — a skirt that fanned would be wide
                // at one end of an edge and nothing at the other.
                for end in [outer[i], outer[j]] {
                    let d = end - inner[i];
                    let across = (dir.x * d.y - dir.y * d.x).abs() / dir.length();
                    assert!(
                        (across - f).abs() < 1e-3,
                        "edge {i}-{j} at {degrees}° is feathered over {across}, not {f}"
                    );
                }
                // And outside, not folded back over the gradient.
                let out = outer[i] - centroid;
                let inn = inner[i] - centroid;
                assert!(out.length() > inn.length(), "corner {i} folded inwards");
            }
        }
    }

    /// A harmony's markers are on the ring at the hues the model answered, and
    /// the base one is exactly where the Wheel mode's hue marker is — the two
    /// are one function, and this is what says so.
    #[test]
    fn a_harmonys_markers_sit_on_the_ring_at_its_own_hues() {
        let (inner, outer) = (40.0, 60.0);
        let mid = (inner + outer) * 0.5;
        for base in [0.0_f32, 47.0, 200.0, 359.0] {
            let hues = Harmony::Complementary.hues(base);
            let at: Vec<Pos2> = hues
                .as_slice()
                .iter()
                .map(|h| ring_point(CENTRE, inner, outer, *h))
                .collect();
            for pt in &at {
                assert!(
                    (apart(*pt, CENTRE) - mid).abs() < 1e-3,
                    "{pt:?} is not on the ring"
                );
            }
            // A complementary is straight across, so the two markers are a
            // diameter apart.
            assert!(
                (apart(at[0], at[1]) - mid * 2.0).abs() < 1e-2,
                "at {base}: {at:?}"
            );
            // And the base marker is the wheel's own, at the same hue.
            let angle = base.to_radians();
            let expected = CENTRE + vec2(angle.cos(), angle.sin()) * mid;
            assert!(apart(at[0], expected) < 1e-3);
        }
    }

    /// Two swatches must never share a pixel: the second drawn would take every
    /// click on the overlap, so one member of the harmony would be unreachable.
    /// And the row has to end where it was given room to, whatever divides it.
    #[test]
    fn the_harmony_swatches_tile_their_row_without_overlapping() {
        for width in [264.0_f32, 190.0, 48.0, 1.0, 0.0] {
            let row = Rect::from_min_size(pos2(10.0, 20.0), vec2(width, HARMONY_ROW));
            for count in 1..=umber_core::harmony::MAX_HUES {
                let cells: Vec<Rect> = (0..count).map(|i| swatch_cell(row, i, count)).collect();
                for cell in &cells {
                    assert!(cell.width() > 0.0 && cell.height() > 0.0, "{cell:?}");
                    assert_eq!(cell.top(), row.top());
                    // The mark's room is reserved on every swatch, so the row
                    // does not change height as the base moves along it.
                    assert_eq!(cell.height(), HARMONY_SWATCH);
                    let mark = swatch_mark(*cell);
                    assert!(mark.top() >= cell.bottom(), "the mark overlaps the colour");
                    assert!(
                        mark.bottom() <= row.bottom() + 1e-3,
                        "the mark runs off the row"
                    );
                    assert_eq!(mark.width(), cell.width());
                }
                for pair in cells.windows(2) {
                    assert!(
                        pair[1].left() >= pair[0].right(),
                        "width {width}, {count} swatches: {pair:?} overlap"
                    );
                }
                // The row is filled exactly, so the last swatch does not stop
                // short of the edge — except where the floor on a cell's width
                // has already taken over, which is a panel too narrow to draw
                // anything usable in.
                if width >= MIN_PICKER {
                    let last = cells.last().expect("at least one");
                    assert!((last.right() - row.right()).abs() < 1e-3, "{last:?}");
                }
            }
        }
    }

    /// The Harmony mode at the panel's real width, at each relation.
    ///
    /// Written rather than asserted for the reason `layers_panel_preview` is:
    /// what can go wrong in a ring, a square, a dropdown and a row that divides
    /// itself four ways is a *layout*, and no assertion about widgets catches
    /// controls drawn over each other. It also answers the one question the
    /// maths cannot: whether four markers on a ring can be told apart.
    ///
    /// ```sh
    /// cargo test -p umber-app harmony_picker_preview -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "writes preview PNGs and wants a GPU; run deliberately"]
    #[cfg(debug_assertions)]
    fn harmony_picker_preview() {
        use crate::docshot;
        use crate::theme::{Palette, metrics};

        let Some(mut stage) = docshot::Stage::new() else {
            eprintln!("no GPU adapter: nothing to draw into. Skipped.");
            return;
        };
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/harmony");
        std::fs::create_dir_all(&dir).expect("create the preview directory");

        let palette = Palette::of(crate::theme::ThemeKind::Graphite);
        for (index, relation) in Harmony::ALL.into_iter().enumerate() {
            let mut harmony = relation;
            let mut hsv = Hsv::new(28.0, 0.72, 0.86);
            let field = vec2(metrics::PANEL - 2.0 * metrics::PANEL_PAD as f32, 300.0);
            let image = stage.shoot(field, 2.0, &palette, palette.dock, |root| {
                harmony_wheel(root, &palette, &mut harmony, &mut hsv);
            });
            let name = format!("{}-{}", index + 1, relation.label().replace(' ', "-"));
            docshot::write_png(&dir.join(format!("{name}.png")), &image).expect("write the png");
        }
        println!("wrote {} shots to {}", Harmony::ALL.len(), dir.display());
    }

    /// A triangle squashed to nothing has no outward direction, and one NaN
    /// vertex is a mesh egui throws away — a picker that has stopped drawing.
    #[test]
    fn a_degenerate_triangle_gets_no_skirt_rather_than_a_nan() {
        let pt = skirt_corner(CENTRE, CENTRE, 1.0);
        assert_eq!(pt, CENTRE);
    }

    /// Whichever way it is turned, the three corners have to stay a triangle:
    /// the barycentric hit test divides by its area, and a degenerate one
    /// returns NaN and freezes the picker.
    #[test]
    fn the_corners_are_never_collinear() {
        let mut angles = WheelAngles::default();
        for degrees in [0.0, 1.0, 45.0, 120.0, 271.0, 359.0] {
            angles.set(WheelShape::Triangle, degrees);
            for rotate in [true, false] {
                for hue in [0.0, 30.0, 90.0, 180.0, 275.0, 359.9] {
                    let (h, w, b) = corners_at(WheelShape::Triangle, rotate, angles, hue);
                    let (x, y, z) = barycentric(CENTRE, h, w, b);
                    assert!(
                        x.is_finite(),
                        "degenerate at angle={degrees} rotate={rotate} hue={hue}"
                    );
                    // The centre is inside, so every weight is positive.
                    assert!(x > 0.0 && y > 0.0 && z > 0.0, "{x} {y} {z}");
                }
            }
        }
    }
}
