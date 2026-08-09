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
//!
//! A wheel carries two controls in one hit area, and they are kept apart by two
//! rules that have to hold together. [`Hub`] is the centre's geometry, handed to
//! the hit test and to the painting by one function, so a press cannot land on a
//! shape that is not drawn there — and it turns with the shape, because both
//! centres carry an angle and the triangle can be turned by the hue itself. And
//! [`settle`] runs **once, on the press frame**, with [`frame`] holding its
//! answer for the rest of the gesture; the shape moving underneath afterwards
//! therefore cannot change what the gesture is. Each was a bug on its own, and
//! the second is the subtler: a press on the ring sets a hue, the hue swings the
//! apex round to meet that very press, and a wheel that asked again would read
//! it as a press on the triangle — hue frozen, marker slammed to the apex.
//!
//! What ends one gesture and begins the next is an **event** — the primary
//! button going down — and never a position. egui's `press_origin` is the
//! pointer's rather than any widget's, so it moves under a second button and is
//! cleared by a second release; asking it whether a new gesture began made a
//! right-click during a ring drag throw the marker at the pointer, and made a
//! new press at a pixel an abandoned gesture had used inherit that gesture's
//! aim.

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

    /// Whether exchanging the light and dark corners means anything here.
    ///
    /// The triangle alone. Its three corners *are* the hue, white and black, so
    /// which of the two is on which side is a real arrangement — and a
    /// different one in every application, which is the whole reason the
    /// control exists. A square's axes are a saturation and a value, with no
    /// corner standing for either, so there is nothing there to exchange: the
    /// row is not drawn rather than drawn disabled, exactly as "Rotate with
    /// hue" is not drawn for the square and for the same reason.
    ///
    /// A `match` rather than a `matches!`, unlike [`Self::follows_hue`] above
    /// it: a third centre has to be a *decision* about whether it has corners
    /// to swap, and `matches!` would quietly answer "no" for one nobody had
    /// thought about.
    fn can_swap_ends(self) -> bool {
        match self {
            Self::Triangle => true,
            Self::Square => false,
        }
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

/// One `Hsv` as the colour egui paints with.
///
/// `pub(crate)` because the theme editor mixes a palette token on this very
/// picker and has to turn its answer back into a `Color32`; a wrapper of its
/// own would be a second `to_srgb_u8` call to keep in step with this one, which
/// is the drift the palette and the clipboard both refuse.
pub(crate) fn hsv_colour(h: f32, s: f32, v: f32) -> Color32 {
    let [r, g, b, _] = Hsv::new(h, s, v).to_color(1.0).to_srgb_u8();
    Color32::from_rgb(r, g, b)
}

/// How far out a press has to be to belong to the hue ring rather than to the
/// centre.
///
/// The hub's outermost 8% steers hue too, so the ring's inner edge is forgiving
/// to grab — but only where the centre's own shape is not already under the
/// pointer, which is why [`settle`] asks [`Hub::contains`] first. At the sizes
/// the picker is actually drawn at, a triangle inscribed at `inner - 3` and a
/// square inscribed in the same circle both put their corners *outside* this
/// radius, so a ring tested first would take every press on the apex: the
/// corner that is the hue, and the one place on the shape somebody aims at
/// deliberately. (Below an `inner` of about 37 the corners fall inside it
/// instead. Nothing depends on which way round it is — the hub is asked first
/// either way — so the ordering is right at every size and this is only the
/// reason it had to be chosen.)
const RING_GRIP: f32 = 0.92;

/// The shape drawn in the middle of a wheel, as both the hit test and the
/// painting read it.
///
/// One value rather than a shape and a hit test that agree by discipline — the
/// transform tool's rule, that a control may never be drawn where the hit test
/// disagrees with it, applied where the shape can be turned. Both centres carry
/// an angle and the triangle can be turned by the hue itself, so a test that was
/// right at one heading would be this bug at another.
///
/// A bounding box is right at *no* heading. Round the triangle it covers the
/// three wide gaps between the shape and the ring; round a square turned 45° it
/// covers twice the square. That is what a press on the hue ring used to be read
/// as — see [`frame`] for the other half of it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Hub {
    /// The saturation/value triangle, by its three corners: full hue, white,
    /// black.
    Triangle(Pos2, Pos2, Pos2),
    /// The saturation/value field, by its centre, its semi-axes before the turn,
    /// and the angle it is turned by.
    Field(Pos2, Vec2, f32),
}

impl Hub {
    /// Whether a press at `pos` landed on the shape that was drawn.
    ///
    /// Asked of the **press** alone. A drag that began inside goes on being the
    /// centre's however far out it wanders — that is what [`clamp_barycentric`]
    /// and [`field_at`]'s clamp are for, and it is how every picker behaves — so
    /// putting this test on each frame of a drag would freeze the marker at the
    /// edge instead of sliding it along. A press and a drag ask different
    /// questions and only one of them is about containment.
    fn contains(self, pos: Pos2) -> bool {
        match self {
            // A degenerate triangle answers NaN, which is neither inside nor
            // out; false is the honest reading and keeps the NaN out of a hue.
            Self::Triangle(a, b, c) => {
                let (x, y, z) = barycentric(pos, a, b, c);
                x.is_finite() && x >= 0.0 && y >= 0.0 && z >= 0.0
            }
            // In the field's own frame, which is what makes this exact at every
            // angle: the same two axes the mesh and the marker are built on.
            Self::Field(centre, half, angle) => {
                let (across, down) = field_axes(angle);
                let d = pos - centre;
                d.dot(across).abs() <= half.x && d.dot(down).abs() <= half.y
            }
        }
    }
}

/// The centre's geometry, from the shape and the ring it is inscribed in.
///
/// One function, so the hit test and the painting cannot disagree about where
/// the shape is. The square is the largest that fits inside the ring —
/// inscribed in the circle, so turning it cannot take it outside and its size
/// is the same at every angle.
///
/// `mirrored` is the triangle's alone and the square arm ignores it — see
/// [`WheelShape::can_swap_ends`]. It is a parameter rather than something read
/// off a setting in here for the reason `base` is one: this function is what
/// both the hit test and the paint call, so anything that moves a corner has to
/// arrive through it or the two can disagree about where that corner is.
fn hub_of(shape: WheelShape, centre: Pos2, inner: f32, base: f32, mirrored: bool) -> Hub {
    match shape {
        WheelShape::Square => Hub::Field(
            centre,
            Vec2::splat((inner * std::f32::consts::FRAC_1_SQRT_2 - 2.0).max(1.0)),
            base,
        ),
        WheelShape::Triangle => {
            let (hue_pt, white_pt, black_pt) =
                triangle_corners(centre, (inner - 3.0).max(1.0), base, mirrored);
            Hub::Triangle(hue_pt, white_pt, black_pt)
        }
    }
}

/// Draw the centre and give it this frame's share of the gesture.
///
/// The two shapes read a point differently — barycentric weights against two
/// axes — but they answer the same question, so the choice is made once here
/// rather than at each of the two call sites.
fn hub_field(ui: &mut Ui, hub: Hub, drag: Option<Pos2>, hsv: &mut Hsv) -> bool {
    match hub {
        Hub::Triangle(hue_pt, white_pt, black_pt) => {
            sv_triangle(ui, hue_pt, white_pt, black_pt, drag, hsv)
        }
        Hub::Field(centre, half, angle) => sv_field(ui, centre, half, angle, drag, hsv),
    }
}

/// Draw the picker. Returns true when the colour changed.
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    p: &Palette,
    mode: PickerMode,
    shape: &mut WheelShape,
    rotate: &mut bool,
    mirrored: &mut bool,
    angles: &mut WheelAngles,
    harmony: &mut Harmony,
    hsv: &mut Hsv,
) -> bool {
    match mode {
        PickerMode::Wheel => wheel(ui, p, shape, rotate, mirrored, angles, hsv),
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
/// wheel's shape, spin, swap, angle or harmony to live — a dialog that drew no
/// control for a setting must not be able to change it, because a change nobody
/// can see made where nobody can undo it is not a control at all. Both used to
/// declare four throwaway locals and call [`show`], which is the same block
/// written twice and one more place to edit every time this signature grows. It
/// is the same `sliders` the mode draws, so the two cannot diverge.
///
/// The theme editor's token picker is deliberately *not* one of these: it draws
/// the whole picker, mode switch and all, so every one of those settings has
/// somewhere to live and is visible where it was changed. There is one colour
/// picker in Umber and it is set up once — see `settings::token_picker`.
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

/// The same answer as radians from egui's zero angle, which is what
/// [`hub_of`], [`field_axes`] and [`triangle_corners`] all take.
///
/// Its own function because the hit test and the painting both need it, and a
/// centre tested at one heading and drawn at another is the whole class of bug
/// [`Hub`] exists to close.
fn wheel_base(shape: WheelShape, rotate: bool, angles: WheelAngles, hue: f32) -> f32 {
    (shape.neutral() + wheel_angle(shape, rotate, angles, hue)).to_radians()
}

/// What egui reported about this frame's gesture on a wheel.
///
/// A plain reading rather than a [`Response`], so that [`frame`] below is a
/// pure function of it — the division `crate::gesture` and `install::detect`
/// keep, and the only way the frames a gesture is made of can be driven without
/// a window.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Reported {
    /// Where the pointer is, if this widget owns a gesture at all. `Some` from
    /// the frame of the press — *before* egui has decided the gesture is a drag
    /// — through to the frame of the release.
    at: Option<Pos2>,
    /// Where the button went down, while it is still down. egui clears it on
    /// the release.
    ///
    /// Read **only** to give a press frame its origin. It is the *pointer's*
    /// and not this widget's — any button moves it and any release clears
    /// it — so it can say where a gesture began and must never be asked whether
    /// one began. That is [`Self::pressed`]'s job.
    press_origin: Option<Pos2>,
    /// Did the **primary** button go down this frame?
    ///
    /// This is the only thing that starts a wheel gesture, so it is the only
    /// honest reading of "a new one began". `any_pressed` would be true for a
    /// right button pressed in the middle of a left drag, which is the bug this
    /// field replaced a position comparison to close. It is safe under touch as
    /// well: `egui-winit` synthesises pointer buttons for the *first* contact
    /// only — it tracks one `pointer_touch_id` — so a second finger cannot
    /// raise it.
    pressed: bool,
}

/// Which control a gesture reached, with no position in it.
///
/// Positionless because it is settled **once**, at the press, and then held —
/// where the pointer is afterwards is this frame's business and which control
/// it is working is not.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Aim {
    /// Neither control: the gaps between an inscribed shape and the ring, and
    /// everything beyond the ring's outer edge. Nothing is drawn in either, so
    /// nothing in either may move a colour.
    #[default]
    Nothing,
    /// The hue ring.
    Ring,
    /// The saturation and value shape.
    Centre,
}

impl Aim {
    /// This aim, working at a position.
    fn at(self, pos: Pos2) -> WheelAim {
        match self {
            Self::Nothing => WheelAim::Idle,
            Self::Ring => WheelAim::Ring(pos),
            Self::Centre => WheelAim::Centre(pos),
        }
    }
}

/// What a wheel does with this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Frame {
    /// No gesture. Anything recorded belongs to one that has ended.
    Idle,
    /// A gesture already settled, now at this position.
    Held(Aim, Pos2),
    /// The first frame of a gesture: settle it from this origin, record the
    /// answer, and work it at this position.
    Press(Pos2, Pos2),
}

/// Which of those three this frame is, given what egui reported and what the
/// widget recorded on an earlier frame of the same gesture.
///
/// The wheel's two controls overlap, so which of them a gesture belongs to is
/// decided **once, at the press**, and held for the whole gesture. That is what
/// lets a hue drag carry on across the middle, what stops a drag out of the
/// centre being handed to the ring when it is let go over one, and — the reason
/// holding it is not merely tidy — what stops the *shape itself* changing the
/// answer. The triangle's apex follows the hue by default, so a press on the
/// ring sets a hue that swings the apex round to meet the very point it was
/// pressed at: re-asking [`Hub::contains`] on the next frame then reads that
/// press as a press on the triangle, the hue freezes, and the saturation and
/// value marker slams to the apex. That is the reported bug, in the 2 px band
/// [`RING_GRIP`] exists to serve, and holding the aim is the whole of the cure.
///
/// Two more frames egui makes awkward, and both were the other half of it. A
/// press is not yet a *drag* — egui waits for the pointer to travel — and a
/// release is no longer one, so a reading that asked for a drag fell through to
/// the pointer's own position on both, and handed a press on the ring to the
/// field. Here the press frame settles the aim and the release frame is `Held`,
/// because egui clears `press_origin` on the release and `held` is what is left.
///
/// A press and a release inside one frame is `Press` with the pointer's own
/// position for the origin, which it is: there has been no frame in which to
/// move.
///
/// **A new gesture is an event, never a position.** This used to compare egui's
/// `press_origin` against the origin the record was settled at, and that is
/// wrong in both directions. It reads a *second* button pressed mid-drag as a
/// new gesture — egui keeps this widget's interaction, so a right-click while
/// dragging the ring re-settled against the live hub, threw saturation and value
/// at the pointer, and did it again on the release; a resting hand on a tablet
/// is the ordinary way to meet that. And it reads a genuinely new press at the
/// *same pixel* as the old gesture, so a record abandoned by switching picker
/// mode mid-drag — where `wheel` is not drawn and so cannot clear it — was
/// inherited, aim and all, by the next press that happened to land there.
/// [`Reported::pressed`] separates the two readings: a record is kept unless the
/// primary button actually went down.
fn frame(reported: Reported, held: Option<Aim>) -> Frame {
    let Some(at) = reported.at else {
        return Frame::Idle;
    };
    match held {
        // Still the gesture this widget settled: nothing has started a new one.
        Some(aim) if !reported.pressed => Frame::Held(aim, at),
        _ => Frame::Press(reported.press_origin.unwrap_or(at), at),
    }
}

/// Which of a wheel's two controls this frame's gesture is working, and where.
#[derive(Clone, Copy, Debug, PartialEq)]
enum WheelAim {
    /// Nothing to do.
    Idle,
    /// The hue ring, at this position.
    Ring(Pos2),
    /// The saturation and value shape, at this position.
    Centre(Pos2),
}

impl WheelAim {
    /// Where the centre should follow the pointer, if it should at all.
    fn centre(self) -> Option<Pos2> {
        match self {
            Self::Centre(pos) => Some(pos),
            Self::Idle | Self::Ring(_) => None,
        }
    }
}

/// Settle a press against the two controls.
///
/// The one place the hub is consulted, which is what makes the answer a
/// property of the press rather than of whatever the shape happens to be doing
/// a frame later — see [`frame`].
///
/// The hub is asked **first** and the ring second: [`RING_GRIP`] lets the ring
/// be grabbed from a little inside its own edge, and both centres reach into
/// that band, so a ring tested first would take every press on a triangle's
/// apex — the corner that *is* the hue.
///
/// And the ring has an outer edge as well as an inner one. The wheel's hit area
/// is the square around it, so its four corners reach `outer × √2` with nothing
/// drawn out there; a press in one used to set a hue. The bound is `outer`
/// exactly, where [`hue_ring`] fades a skirt one feather *past* it — so the one
/// faded pixel at the rim takes no press. That is the conservative direction: it
/// is a pixel the ring is already handing back to the background, and matching
/// the skirt would mean the hit test tracking a number that exists only to
/// antialias.
fn settle(centre: Pos2, inner: f32, outer: f32, hub: Hub, from: Pos2) -> Aim {
    if hub.contains(from) {
        return Aim::Centre;
    }
    let out = (from - centre).length();
    if out > inner * RING_GRIP && out <= outer {
        Aim::Ring
    } else {
        Aim::Nothing
    }
}

/// [`frame`] and [`settle`], against what egui and the previous frame have to
/// say.
///
/// One frame of a wheel gesture: what to do now, and what to remember.
///
/// **The whole of the dispatch, in one place.** [`wheel_aim`] and the tests used
/// to hold a copy each, and the copy was exactly the thing this file's bug lives
/// in: re-settling the aim per frame could be put back into the *shipped* path
/// with the whole suite green, because the guard only ever drove the harness's
/// twin. A rule stated twice is the drift `blend.wgsl` and `render_float` are
/// each one-of-a-kind for.
///
/// The aim to keep comes back rather than being stored here, because the two
/// callers store it differently — egui's per-widget memory in the picker, a
/// field in the tests — and *where* it is kept is the only part that genuinely
/// differs.
fn resolve(
    reported: Reported,
    held: Option<Aim>,
    centre: Pos2,
    inner: f32,
    outer: f32,
    hub: Hub,
) -> (Option<Aim>, WheelAim) {
    match frame(reported, held) {
        Frame::Idle => (None, WheelAim::Idle),
        Frame::Held(aim, at) => (Some(aim), aim.at(at)),
        Frame::Press(from, at) => {
            let aim = settle(centre, inner, outer, hub, from);
            (Some(aim), aim.at(at))
        }
    }
}

/// [`resolve`], reading egui and keeping its answer in this widget's memory.
///
/// The settled aim is recorded on its press frame and cleared on the first frame
/// this widget owns no gesture. A record can therefore outlive its gesture only
/// where no such frame ran — switching picker mode mid-drag, since `wheel` is
/// then not drawn at all — and a stale one is harmless because [`frame`] settles
/// afresh whenever the primary button goes down.
fn wheel_aim(
    ui: &Ui,
    id: egui::Id,
    response: &Response,
    centre: Pos2,
    inner: f32,
    outer: f32,
    hub: Hub,
) -> WheelAim {
    let reported = ui.ctx().input(|i| Reported {
        at: response.interact_pointer_pos(),
        press_origin: i.pointer.press_origin(),
        pressed: i.pointer.primary_pressed(),
    });
    let held = ui.ctx().data_mut(|d| d.get_temp::<Aim>(id));
    let (keep, aimed) = resolve(reported, held, centre, inner, outer, hub);
    ui.ctx().data_mut(|d| match keep {
        // Written when it changes, which is once per gesture: the aim cannot
        // move within one, and `insert_temp` boxes what it is given.
        Some(aim) if held != Some(aim) => {
            d.insert_temp(id, aim);
        }
        None => {
            d.remove_temp::<Aim>(id);
        }
        Some(_) => {}
    });
    aimed
}

fn wheel(
    ui: &mut Ui,
    p: &Palette,
    shape: &mut WheelShape,
    rotate: &mut bool,
    mirrored: &mut bool,
    angles: &mut WheelAngles,
    hsv: &mut Hsv,
) -> bool {
    let mut changed = false;

    let size = ui.available_width().clamp(MIN_PICKER, 176.0);
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width().max(size), size), Sense::hover());
    let area = Rect::from_center_size(rect.center(), vec2(size, size));
    let centre = area.center();
    let (inner, outer) = ring_radii(size);

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
    let id = ui.id().with("wheel");
    let response = ui.interact(area, id, Sense::click_and_drag());

    // The centre as it stands *now*, which is the shape a press this frame
    // landed on. With "Rotate with hue" on, the hue the ring is about to be
    // given belongs to a triangle nobody has seen yet, and judging a press
    // against that one would be judging it by where the shape is going. It is
    // read only on a press frame — see `frame` — so the shape swinging round
    // afterwards cannot take the gesture off the ring.
    let aimed = wheel_aim(
        ui,
        id,
        &response,
        centre,
        inner,
        outer,
        hub_of(
            *shape,
            centre,
            inner,
            wheel_base(*shape, *rotate, *angles, hsv.h),
            // The swap makes no difference to *this* hub and is handed over
            // anyway. It is a reflection about the axis through the hue corner,
            // so the three points are the same three points and
            // `Hub::contains` cannot tell them apart — demonstrated by
            // mutation: pass `false` here and all 689 tests stay green. What it
            // buys is that the press is judged against the shape that is
            // drawn, by construction rather than by an argument about
            // symmetry — which is exactly what `Hub` exists for, and what would
            // stop holding the day somebody makes the swap a rotation.
            *mirrored,
        ),
    );

    // --- hue ring ---
    if let WheelAim::Ring(pos) = aimed {
        hsv.h = ring_hue(centre, pos);
        changed = true;
    }

    hue_ring(ui, centre, inner, outer);

    // Hue marker, on the middle of the ring wherever the ring ended up.
    ui.painter()
        .circle_stroke(ring_point(centre, inner, outer, hsv.h), 6.0, MARKER_STROKE);

    // --- saturation / value shape ---
    //
    // Rebuilt from the hue as it now is, so a triangle that follows the hue
    // turns on the frame the ring is dragged rather than a frame behind the
    // marker that turned it.
    let base = wheel_base(*shape, *rotate, *angles, hsv.h);
    changed |= hub_field(
        ui,
        hub_of(*shape, centre, inner, base, *mirrored),
        aimed.centre(),
        hsv,
    );

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

    // Which of the two lower corners is white and which is black.
    //
    // Every application that draws this triangle picks one and does not say
    // which, so somebody arriving from another one reaches for the tint they
    // want and finds the shade — a mistake that is invisible until the stroke
    // is down, because both arrangements look equally right.
    //
    // The **label is what it does, not what it is**. Geometrically this is a
    // mirror of the triangle about the axis through the hue corner; because the
    // shape is equilateral and that axis runs through one corner, the outline
    // does not move and the only visible effect is that white and black change
    // places. "Mirror" would be true and would leave somebody guessing which of
    // the three corners it turned about.
    //
    // Separate from the Angle rail rather than folded into it, and that is the
    // whole reason it had to exist: the rail turns all three corners together,
    // so no angle whatever reaches the mirrored arrangement. Rotation gives
    // three of the six ways to label the corners and reflection gives the other
    // three; the pair reaches all of them, which is why neither control needs
    // to grow.
    //
    // Triangle only — see `WheelShape::can_swap_ends` — and not drawn at all
    // for the square, like the row above it.
    if shape.can_swap_ends() {
        crate::widgets::toggle_row(ui, p, "Swap white and black", mirrored);
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
    // A slider rather than a drag on the shape itself. The two things drawn on
    // the wheel are both spoken for — the ring steers hue and the shape sets
    // saturation and value — so a rotate gesture could only be a modifier, or
    // the gaps between the shape and the ring, which take no press at all now
    // that `Hub::contains` decides. Neither is something anyone would find
    // without being told, and the gaps least of all: nothing is drawn in them,
    // and round a square they are four slivers.
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
            .on_hover_text("The hue is setting the angle. Turn Rotate with hue off to set it.");
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

/// The hue ring's two radii for a wheel of the given side.
///
/// One function because both wheels want the same ring, and because
/// `the_base_ring_stays_on_the_hue_band` has to be able to ask for the band a
/// real wheel has rather than recomputing the expression it is checking — a
/// test that copies the formula agrees with whatever the formula becomes.
///
/// The ring is a fixed thickness, so a small enough wheel would have its inner
/// edge outside its outer one; below about 54 points the hub is kept instead
/// and the band narrows.
fn ring_radii(size: f32) -> (f32, f32) {
    let outer = size * 0.5;
    ((outer - RING_THICKNESS).max(outer * 0.25), outer)
}

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

/// What saturation and value a point in the triangle stands for — [`field_at`]
/// for the other centre, and `None` where the three corners are not a triangle
/// at all.
///
/// Clamped rather than refused, for the reason [`clamp_barycentric`] gives: a
/// drag past an edge slides along it instead of freezing the picker. Whether a
/// point is *in* the triangle is [`Hub::contains`]'s question and is asked of
/// the press alone — these are two different questions and the clamp is not an
/// answer to the first.
fn triangle_at(hue_pt: Pos2, white_pt: Pos2, black_pt: Pos2, pos: Pos2) -> Option<(f32, f32)> {
    // Barycentric coordinates give saturation and value directly.
    let (a, b, c) = barycentric(pos, hue_pt, white_pt, black_pt);
    if !a.is_finite() {
        return None;
    }
    let (a, b, _) = clamp_barycentric(a, b, c);
    let v = (a + b).clamp(0.0, 1.0);
    let s = if v > 1e-4 {
        (a / v).clamp(0.0, 1.0)
    } else {
        0.0
    };
    Some((s, v))
}

/// Saturation/value triangle, drawn at the three corners [`hub_of`] placed.
///
/// The apex is the full hue, with white and black at the other two corners.
/// Where that apex points is the hue when the shape is following it, which is
/// what the design shows, and otherwise the shape's neutral pose turned by the
/// user's own angle.
///
/// The choice is not cosmetic. Following the hue keeps the apex next to the
/// marker that sets it, so the two controls read as one instrument — but it
/// also means the whole saturation/value field swings under the pointer while
/// the ring is being dragged, so a tint chosen at one hue is somewhere else at
/// the next. Holding still gives up the first to get the second: the point you
/// last picked stays where you left it, and picking the same tint across
/// several hues becomes a matter of returning to the same place.
///
/// The corners are passed in rather than built here, because the hit test needs
/// exactly these three points — see [`Hub`].
///
/// `drag` is where the pointer is, if this frame's gesture belongs to the
/// centre. The wheel decides that — see the interaction comment there — so
/// there is no `interact` of its own to overlap the ring's.
fn sv_triangle(
    ui: &mut Ui,
    hue_pt: Pos2,
    white_pt: Pos2,
    black_pt: Pos2,
    drag: Option<Pos2>,
    hsv: &mut Hsv,
) -> bool {
    let mut changed = false;

    if let Some(pos) = drag
        && let Some((s, v)) = triangle_at(hue_pt, white_pt, black_pt, pos)
    {
        hsv.s = s;
        hsv.v = v;
        changed = true;
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
/// Its own function because it is the whole of what the angle and the swap
/// change, and everything else about the triangle — the hit test, the mesh, the
/// marker — is derived from these three points. Testing it without a `Ui` is
/// therefore testing the feature. [`Hub`] is what carries them from here to
/// both.
///
/// `mirrored` exchanges the white and black corners, which is what the Swap
/// white and black control asks for. It is spelled as running the corners the
/// other way round the circle rather than as two `if`s over the returned
/// points, because that is what it *is*: a reflection about the axis through
/// the hue corner. That the outline is unmoved and only two roles change hands
/// then falls out of the arithmetic instead of being a property somebody has to
/// keep true — and it composes with `base` for free, so the angle rail and the
/// swap together reach all six arrangements of the three corners rather than
/// three each.
fn triangle_corners(centre: Pos2, radius: f32, base: f32, mirrored: bool) -> (Pos2, Pos2, Pos2) {
    let step = if mirrored { -1.0 } else { 1.0 };
    let corner = |k: f32| {
        let a = base + step * k * std::f32::consts::TAU / 3.0;
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

/// The hue a position along the Square mode's bar stands for — [`ring_hue`]'s
/// opposite number, and its own function for the same reason: it is the other
/// place a gesture writes a hue, and it has two rules that a later tidy-up would
/// otherwise read as one redundant one.
///
/// `t` reaches exactly 1.0 — it is clamped there, and the pointer can be dragged
/// past the edge — so `t * 360.0` is exactly 360, the one value outside the
/// range `Hsv` documents. `wrap_hue` alone is not the answer: a whole turn is
/// the same hue as none, so the right-hand end would store 0 and the knob, which
/// is drawn *from the hue*, would jump to the far left while the pointer was at
/// the far right. Stopping one representable step short is the same red, inside
/// the range, and under the hand. The wrap stays behind it as the door every hue
/// comes through.
fn bar_hue(t: f32) -> f32 {
    umber_core::color::wrap_hue(t.clamp(0.0, 1.0 - f32::EPSILON) * 360.0)
}

/// Saturation/value square with a hue bar beneath.
fn square(ui: &mut Ui, _p: &Palette, hsv: &mut Hsv) -> bool {
    let mut changed = false;
    let width = ui.available_width().max(MIN_PICKER);

    let (rect, response) = ui.allocate_exact_size(vec2(width, 130.0), Sense::click_and_drag());
    // Nothing overlaps this one, so it does its own interacting — unlike the
    // wheel's centre, which shares a hit area with the ring, and it keeps
    // egui's own reading rather than the wheel's.
    //
    // That asymmetry is deliberate and it is worth saying exactly what it costs,
    // because the wheel now answers on the press frame and this does not: here a
    // press does nothing until the pointer has travelled egui's click threshold
    // or the button comes up. The wheel needed a press-frame answer because it
    // has *two* controls in one hit area and a gesture has to be attributed to
    // one of them before it can be carried out; this field has nothing to be
    // told apart from, so there is no attribution to make early and the lag is
    // the same one every other egui drag in the interface has. Making it match
    // would be a change to how two more controls feel, off the back of a bug
    // report about neither.
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
        hsv.h = bar_hue((pos.x - bar.left()) / bar.width().max(1.0));
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

/// The radius of a member's marker on the hue ring.
///
/// The same 6 the Wheel mode's hue marker uses, deliberately and for the reason
/// [`MARKER_STROKE`] is one value: every member of a harmony *is* a hue marker,
/// so a harmony's markers being a size of their own would read as a different
/// instrument. Every member gets this, the base included — a marker says where a
/// hue is, and one member's saying it in a different size would say something
/// about the colour instead.
const HARMONY_MARKER: f32 = 6.0;

/// How far outside its own marker the base's second, concentric ring sits.
///
/// This is what says which member is the colour in hand, now that no member is
/// filled. It has to be a difference of *mark*: at zero saturation every member
/// of a harmony is the same grey, so nothing about the colour can tell them
/// apart. A ring *outside* rather than inside, because an inner ring at this
/// radius is three pixels across at an ordinary scale factor and reads as a dot
/// — which is the fill this change exists to remove, put back smaller.
///
/// The figure is what keeps the outer ring inside the hue band rather than
/// hanging off it: the band is [`RING_THICKNESS`] wide wherever the wheel is
/// wide enough for that — which is every size from 54 points up, so every size
/// a panel is actually drawn at — and `HARMONY_MARKER + HARMONY_BASE_GAP` plus
/// half of [`MARKER_STROKE`] is exactly half of it. Below that the inner radius
/// is clamped and the band narrows, so on a wheel at the [`MIN_PICKER`] floor
/// the ring overhangs the rim by a point. That is left alone rather than solved:
/// it is a panel dragged to nothing, and a white ring a point over the rim is
/// still a white ring. `the_base_ring_stays_on_the_hue_band` measures both.
const HARMONY_BASE_GAP: f32 = 3.0;

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
    let (inner, outer) = ring_radii(size);

    // One interaction for the whole wheel, settled at the press, for exactly
    // the reason the Wheel mode's is: the centre's rect covers the ring at the
    // four diagonals, so two overlapping `interact` rects would send a hue drag
    // begun there to the saturation and value field.
    let id = ui.id().with("harmony-wheel");
    let response = ui.interact(area, id, Sense::click_and_drag());

    // The saturation and value field, level and inscribed in the ring — the
    // Wheel mode's square at its neutral, through the one function that places
    // it, so the two modes cannot end up with differently sized centres. Built
    // before the ring is read because the press is judged against it, and it
    // does not move when the hue does.
    // `false` for the swap: this centre is the square, which has no corners to
    // exchange, and `hub_of` ignores it there. Stated rather than threaded from
    // a setting, because this mode draws no triangle and so has no control for
    // one.
    let hub = hub_of(WheelShape::Square, centre, inner, 0.0, false);
    let aimed = wheel_aim(ui, id, &response, centre, inner, outer, hub);

    if let WheelAim::Ring(pos) = aimed {
        hsv.h = ring_hue(centre, pos);
        changed = true;
    }

    hue_ring(ui, centre, inner, outer);

    changed |= hub_field(ui, hub, aimed.centre(), hsv);

    ui.add_space(8.0);

    // Which relation. A dropdown rather than a segmented control: six names,
    // the longest of them "Tetrad (rectangle)", in a panel 264 px wide.
    //
    // **Outlined**, which is the one thing this row was missing, and it took two
    // goes. `widgets::dropdown` draws no fill — see its own docs — so a trigger
    // alone on a line, with nothing before it, is a word and a small chevron on
    // the panel's own background, and it was read as a *caption saying which
    // relation had been drawn* rather than as the control that picks one.
    // Reported by an artist asking for a triad and a tetrad, both of which were
    // already in this menu.
    //
    // The first repair was a dim "Relation" caption above it, and it was
    // reported again: a dim word over a plain word is a labelled read-only
    // field, so the caption made the pair read *more* like a readout, not less.
    // The affordance has to be on the control itself. A border is the answer and
    // is not the filled variant the module refuses — see `Dropdown::outlined`,
    // where that distinction is argued — and it needs no caption, because a box
    // with a chevron in it is already the shape of "pick one".
    let mut picked = *harmony;
    crate::widgets::dropdown(
        ui,
        p,
        crate::widgets::Dropdown::new(harmony.label())
            .width(crate::widgets::DropdownWidth::Fill)
            // The panel body's own surface, which is what the border is derived
            // against. `dock` rather than `window`: this picker is drawn in a
            // module body, and the theme editor can set either to anything.
            .outlined(p.dock),
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
    // **Every member wears the same open ring the Wheel mode's hue marker
    // does**, at the same radius, with the wheel's own hue showing through it.
    // The others used to be smaller *filled* discs of the member's colour at the
    // current saturation and value, which paints a muddy disc over the vivid hue
    // underneath — reported, and right: a marker on a hue ring says *where* a
    // hue is, and filling it with a duller version of that hue is a swatch
    // pretending to be a marker. The swatch row below is where the colours are.
    //
    // The comment this replaces defended the fill on the ground that "which one
    // is in hand is a difference of mark rather than of colour", because at zero
    // saturation every member is the same grey. That is a real constraint and it
    // survives: the base is still told apart by its **mark**, a second ring
    // outside the first. Two concentric rings can only mean "this one", they
    // read at any saturation because both are white on whatever the wheel holds,
    // and neither of them hides a hue. What is gone is only the *fill*.
    let painter = ui.painter();
    for (index, hue) in harmony.hues(hsv.h).as_slice().iter().enumerate() {
        let at = ring_point(centre, inner, outer, *hue);
        painter.circle_stroke(at, HARMONY_MARKER, MARKER_STROKE);
        if index == 0 {
            painter.circle_stroke(at, HARMONY_MARKER + HARMONY_BASE_GAP, MARKER_STROKE);
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
            .on_hover_text(format!("{hint} · #{r:02X}{g:02X}{b:02X}"))
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
        triangle_corners(
            CENTRE,
            RADIUS,
            wheel_base(shape, rotate, angles, hue),
            false,
        )
    }

    /// The ring the widest wheel the picker will draw actually has.
    ///
    /// `wheel` clamps its side to 176, halves it for `outer`, and takes
    /// `outer - RING_THICKNESS` for `inner` — so these are the real numbers and
    /// not a convenient pair. That matters: every margin checked below is
    /// checked at a size the picker can be at, and the margins are narrower
    /// here than at a made-up radius.
    const OUTER: f32 = 176.0 * 0.5;
    const INNER: f32 = OUTER - RING_THICKNESS;

    /// The centre as [`wheel`] builds it: a shape at an angle, through the one
    /// function that places it.
    fn hub_at(shape: WheelShape, degrees: f32) -> Hub {
        let mut angles = WheelAngles::default();
        angles.set(shape, degrees);
        hub_of(
            shape,
            CENTRE,
            INNER,
            wheel_base(shape, false, angles, 0.0),
            false,
        )
    }

    /// A point on the hue ring, at a given fraction across the band.
    ///
    /// Zero is the ring's inner edge, which is the *closest in* a press can be
    /// and still be unambiguously the ring's: every shape `hub_of` builds is
    /// inscribed inside it. Nearer the middle than that is the grip band, which
    /// both shapes' corners reach into deliberately.
    fn on_the_ring(degrees: f32, across: f32) -> Pos2 {
        let a = degrees.to_radians();
        CENTRE + vec2(a.cos(), a.sin()) * (INNER + (OUTER - INNER) * across)
    }

    /// The extreme points of a centre — the three corners of a triangle, the
    /// four of a field. These are what reach past [`RING_GRIP`], so they are
    /// what says the hub has to be asked before the ring.
    fn hub_corners(hub: Hub) -> Vec<Pos2> {
        match hub {
            Hub::Triangle(a, b, c) => vec![a, b, c],
            Hub::Field(centre, half, angle) => {
                let (across, down) = field_axes(angle);
                [(1.0, 1.0), (1.0, -1.0), (-1.0, 1.0), (-1.0, -1.0)]
                    .iter()
                    .map(|(u, w)| centre + across * (half.x * u) + down * (half.y * w))
                    .collect()
            }
        }
    }

    /// [`wheel`]'s own loop with the `Ui` and the painting taken out: the hub
    /// rebuilt from the hue every frame exactly as the real one rebuilds it,
    /// the frame settled, then whichever of the picker's own readers the aim
    /// names.
    ///
    /// Rebuilding the hub per frame is the point rather than an accident. A
    /// triangle following the hue *moves while the ring is being dragged*, and
    /// a test that held one still could not see the gesture being taken off the
    /// ring by the shape swinging round to meet the press. Nothing here is a
    /// copy of the arithmetic: `hub_of`, `wheel_base`, `frame`, `settle`,
    /// `ring_hue`, `triangle_at` and `field_at` are the functions the running
    /// picker calls.
    struct Wheel {
        shape: WheelShape,
        rotate: bool,
        /// The Swap white and black setting, carried here so a test can drive
        /// the swapped triangle through the same dispatch the picker uses
        /// rather than through `triangle_corners` alone. `new` leaves it off,
        /// which is what every test written before the setting existed assumes.
        mirrored: bool,
        angles: WheelAngles,
        held: Option<Aim>,
        hsv: Hsv,
    }

    impl Wheel {
        fn new(shape: WheelShape, rotate: bool, hsv: Hsv) -> Self {
            Self {
                shape,
                rotate,
                mirrored: false,
                angles: WheelAngles::default(),
                held: None,
                hsv,
            }
        }

        fn hub(&self) -> Hub {
            hub_of(
                self.shape,
                CENTRE,
                INNER,
                wheel_base(self.shape, self.rotate, self.angles, self.hsv.h),
                self.mirrored,
            )
        }

        fn step(&mut self, reported: Reported) -> WheelAim {
            // `resolve` and not a copy of it: the dispatch that holds the aim is
            // the thing under test, so a harness with its own would let the
            // shipped one be broken with every assertion below still passing.
            let (keep, aimed) = resolve(reported, self.held, CENTRE, INNER, OUTER, self.hub());
            self.held = keep;
            match aimed {
                WheelAim::Idle => {}
                WheelAim::Ring(pos) => self.hsv.h = ring_hue(CENTRE, pos),
                // Rebuilt after the hue, as `wheel` does.
                WheelAim::Centre(pos) => match self.hub() {
                    Hub::Triangle(a, b, c) => {
                        if let Some((s, v)) = triangle_at(a, b, c, pos) {
                            self.hsv.s = s;
                            self.hsv.v = v;
                        }
                    }
                    Hub::Field(centre, half, angle) => {
                        (self.hsv.s, self.hsv.v) = field_at(centre, half, angle, pos);
                    }
                },
            }
            aimed
        }

        fn press(&mut self, at: Pos2) -> WheelAim {
            self.step(Reported {
                at: Some(at),
                press_origin: Some(at),
                pressed: true,
            })
        }

        fn drag(&mut self, from: Pos2, at: Pos2) -> WheelAim {
            self.step(Reported {
                at: Some(at),
                press_origin: Some(from),
                pressed: false,
            })
        }

        /// The release, on which egui has already cleared its own origin.
        fn release(&mut self, at: Pos2) -> WheelAim {
            self.step(Reported {
                at: Some(at),
                press_origin: None,
                pressed: false,
            })
        }

        /// A *second* button pressed mid-drag. egui keeps this widget's
        /// interaction, moves `press_origin` to wherever the pointer is, and
        /// does not raise `primary_pressed` — the primary button never came up.
        fn second_button_down(&mut self, at: Pos2) -> WheelAim {
            self.step(Reported {
                at: Some(at),
                press_origin: Some(at),
                pressed: false,
            })
        }

        /// And its release, which clears egui's origin while the primary button
        /// is still down.
        fn second_button_up(&mut self, at: Pos2) -> WheelAim {
            self.step(Reported {
                at: Some(at),
                press_origin: None,
                pressed: false,
            })
        }

        /// A frame on which this widget owns no gesture, which is what clears
        /// the record.
        fn idle(&mut self) -> WheelAim {
            self.step(Reported::default())
        }
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

    /// The Square mode's bar is the other place a gesture writes a hue, and its
    /// far end is the same trap by a different route.
    ///
    /// The knob is drawn from the hue, so the right-hand end has to store a hue
    /// that is still at the right-hand end. `wrap_hue` alone would store zero
    /// and send it to the far left; no clamp at all would store 360, which the
    /// range excludes.
    #[test]
    fn the_hue_bars_far_end_stays_at_its_far_end() {
        assert_eq!(bar_hue(0.0), 0.0);
        assert!(
            (359.0..360.0).contains(&bar_hue(1.0)),
            "the far right read as {}",
            bar_hue(1.0)
        );
        // Dragged past the edge is the same as the edge, not a wrap round to
        // red at the left.
        assert_eq!(bar_hue(1.5), bar_hue(1.0));
        assert_eq!(bar_hue(-0.5), 0.0);
        // Every answer is a hue, and the far end is still red.
        for step in 0..=100 {
            let h = bar_hue(step as f32 / 100.0);
            assert!((0.0..360.0).contains(&h), "{h} is not a hue");
        }
        assert_eq!(
            Hsv::new(bar_hue(1.0), 1.0, 1.0).to_color(1.0).to_srgb_u8(),
            [255, 0, 0, 255],
            "the far end is red, as the gradient under it is"
        );
    }

    /// The bug this file was opened for: turning the hue ring moved the
    /// saturation and value marker, towards whatever direction the ring had been
    /// grabbed from.
    ///
    /// A whole turn, a degree at a time, from a press on the ring — the press
    /// frame, every frame of the drag and the release are all one gesture and
    /// all of them are in here. Saturation and value have to come out **bit for
    /// bit** what they went in as: this is arithmetic that either runs or does
    /// not, so anything short of exact would mean the field had taken a frame.
    ///
    /// Driven with "Rotate with hue" both off and **on**, which is the default
    /// and the case that matters: with it on the triangle's apex chases the hue
    /// the ring is setting, so a wheel that re-settled its aim per frame reads
    /// the press as a press on the triangle the moment the apex reaches it.
    ///
    /// Pressed at several headings and at the ring's *inner* edge, which is
    /// both the press a hit region one size too large takes — the box round a
    /// square turned 37° reaches past the ring at its four diagonals — and the
    /// press the apex can swing round to.
    #[test]
    fn turning_the_hue_through_a_whole_circle_leaves_the_saturation_and_value_alone() {
        for shape in WheelShape::ALL {
            for rotate in [false, true] {
                for pressed_at in [0.0_f32, 45.0, 137.0, 250.0] {
                    let start = Hsv::new(0.0, 0.375, 0.625);
                    let mut wheel = Wheel::new(shape, rotate, start);
                    let press = on_the_ring(pressed_at, 0.0);
                    let mut hues = Vec::new();

                    let check = |aimed: WheelAim, wheel: &Wheel, what: &str| {
                        assert!(
                            matches!(aimed, WheelAim::Ring(_)),
                            "{shape:?} rotate={rotate} pressed at {pressed_at}°: \
                             {what} left the ring as {aimed:?}"
                        );
                        assert_eq!(
                            (wheel.hsv.s, wheel.hsv.v),
                            (start.s, start.v),
                            "{shape:?} rotate={rotate} pressed at {pressed_at}°: \
                             {what} moved the marker"
                        );
                    };

                    let aimed = wheel.press(press);
                    check(aimed, &wheel, "the press");
                    for step in 0..=360 {
                        let aimed = wheel.drag(press, on_the_ring(step as f32, 0.5));
                        check(aimed, &wheel, "a drag frame");
                        assert!(
                            (0.0..360.0).contains(&wheel.hsv.h),
                            "{} is not a hue",
                            wheel.hsv.h
                        );
                        hues.push(wheel.hsv.h.round() as i32);
                    }
                    // A drag begun on the ring carries on across the middle,
                    // and is still the ring's when it is let go there.
                    let aimed = wheel.drag(press, CENTRE);
                    check(aimed, &wheel, "crossing the middle");
                    let aimed = wheel.release(CENTRE);
                    check(aimed, &wheel, "the release");

                    // And the hue went where it was dragged: a sweep that
                    // reached the whole circle, rather than a control that
                    // holds still and passes the assertions above for free.
                    hues.sort_unstable();
                    hues.dedup();
                    assert!(hues.len() > 300, "the sweep reached {} hues", hues.len());
                }
            }
        }
    }

    /// The narrow case defect-hunting found, on its own and stated in full.
    ///
    /// A press in the band between [`RING_GRIP`]'s forgiving edge and the ring
    /// itself is the ring's. With "Rotate with hue" on, that press sets a hue,
    /// and the hue points the apex *at the press*: the pressed point is then
    /// inside the triangle. A wheel that asked again would hand the rest of the
    /// gesture to the saturation and value field, the hue would freeze at the
    /// angle of the press, and the marker would slam to the apex — which is the
    /// reported bug, reached by clicking a 2 px annulus.
    #[test]
    fn a_hue_the_press_itself_set_cannot_turn_the_shape_onto_that_press() {
        let start = Hsv::new(0.0, 0.4, 0.6);
        // Strictly inside the grip's own edge and no further out than the
        // triangle's circumradius: that band, `(0.92 × inner, inner − 3]`, is
        // the whole of where the trap can be sprung.
        for across in [0.005_f32, 0.015, 0.03] {
            // Just inside the ring's inner edge — inside the triangle's own
            // circumradius, which is what makes the case reachable at all.
            let at = |degrees: f32| {
                let a = degrees.to_radians();
                CENTRE + vec2(a.cos(), a.sin()) * (INNER * RING_GRIP + across * INNER)
            };
            // 340° crosses the wrap on the way round, which is the case a bare
            // subtraction of hues gets wrong.
            for degrees in [7.0_f32, 100.0, 250.0, 340.0] {
                let press = at(degrees);
                assert!(
                    (press - CENTRE).length() > INNER * RING_GRIP,
                    "that is not the grip band"
                );
                let mut wheel = Wheel::new(WheelShape::Triangle, true, start);
                assert!(matches!(wheel.press(press), WheelAim::Ring(_)));
                // The apex is now at `degrees`, so the press is inside the
                // triangle — which is exactly the trap.
                assert!(
                    wheel.hub().contains(press),
                    "at {degrees}° the apex did not reach the press, so this \
                     proves nothing"
                );
                for step in 1..=8 {
                    let aimed = wheel.drag(press, at(degrees + step as f32 * 5.0));
                    assert!(
                        matches!(aimed, WheelAim::Ring(_)),
                        "{degrees}°, frame {step}: the triangle took the drag"
                    );
                }
                assert_eq!((wheel.hsv.s, wheel.hsv.v), (start.s, start.v));
                // Round the circle, not along a line: `340 + 40` is 20, and a
                // bare subtraction would call that a hue that had not moved.
                let apart = (wheel.hsv.h - degrees).abs();
                let moved = apart.min(360.0 - apart);
                assert!(moved > 30.0, "the hue froze at {}", wheel.hsv.h);
            }
        }
    }

    /// The press has to land on the shape that is *drawn*, not on the square
    /// around the ring it is inscribed in — the transform tool's rule, that a
    /// control may never be drawn where the hit test disagrees with it.
    ///
    /// Driven at 37° and 214° as well as the level poses, because both centres
    /// can be turned and an axis-aligned bounding box passes at 0° and 90°
    /// whatever is behind it.
    #[test]
    fn only_the_shape_that_is_drawn_takes_a_press() {
        for shape in WheelShape::ALL {
            for degrees in [0.0_f32, 37.0, 90.0, 214.0] {
                let hub = hub_at(shape, degrees);
                assert!(hub.contains(CENTRE), "{shape:?} at {degrees}° is empty");
                // Nowhere on the ring is inside the centre, at any heading and
                // anywhere across the band. Stopping a hair short of the outer
                // edge rather than landing on it: `length()` of a point placed
                // by `cos`/`sin` at exactly that radius rounds either way, and
                // a test that flickers on the boundary tests the boundary
                // rather than the rule. The rule at the edge is the sweep
                // below.
                for across in [0.0_f32, 0.5, 0.98] {
                    for step in 0..72 {
                        let at = on_the_ring(step as f32 * 5.0, across);
                        assert!(
                            !hub.contains(at),
                            "{shape:?} at {degrees}° reaches the ring at {}°, {across} across",
                            step * 5
                        );
                        assert_eq!(
                            settle(CENTRE, INNER, OUTER, hub, at),
                            Aim::Ring,
                            "{shape:?} at {degrees}°: a press on the ring at {}° went elsewhere",
                            step * 5
                        );
                    }
                }
                // Past the ring's *outer* edge nothing is drawn either, and the
                // wheel's hit area is the square around it — so its four
                // corners reach `outer × √2` and used to set a hue.
                for step in 0..72 {
                    let out = on_the_ring(step as f32 * 5.0, 1.0);
                    let past = CENTRE + (out - CENTRE) * 1.02;
                    assert_eq!(
                        settle(CENTRE, INNER, OUTER, hub, past),
                        Aim::Nothing,
                        "{shape:?} at {degrees}°: outside the ring at {}° set a hue",
                        step * 5
                    );
                }
                // And the shape's own corners are its own, which is the whole
                // reason the hub is asked before the ring: they reach past
                // `RING_GRIP`'s forgiving inner edge.
                for corner in hub_corners(hub) {
                    let just_inside = CENTRE + (corner - CENTRE) * 0.98;
                    assert!(
                        (just_inside - CENTRE).length() > INNER * RING_GRIP,
                        "{shape:?} at {degrees}°: this corner does not reach the grip, so \
                         the ordering is untested"
                    );
                    assert_eq!(
                        settle(CENTRE, INNER, OUTER, hub, just_inside),
                        Aim::Centre,
                        "{shape:?} at {degrees}°: the ring took a press on a corner"
                    );
                }
            }
        }
    }

    /// A drag that began inside the centre keeps it however far out it wanders,
    /// and goes on sliding along the edge. Containment is the *press*'s
    /// question; asking it again every frame would freeze the marker where the
    /// pointer left the shape, and the clamp exists precisely so it does not.
    #[test]
    fn a_drag_out_of_the_centre_still_belongs_to_the_centre() {
        for shape in WheelShape::ALL {
            let mut wheel = Wheel::new(shape, true, Hsv::new(123.0, 0.5, 0.5));
            assert!(matches!(wheel.press(CENTRE), WheelAim::Centre(_)));
            let mut seen = Vec::new();
            for step in 0..36 {
                let a = (step as f32 * 10.0).to_radians();
                let at = CENTRE + vec2(a.cos(), a.sin()) * INNER * 2.0;
                let aimed = wheel.drag(CENTRE, at);
                assert!(
                    matches!(aimed, WheelAim::Centre(_)),
                    "{shape:?} lost the drag at {}° as {aimed:?}",
                    step * 10
                );
                assert_eq!(wheel.hsv.h, 123.0, "the hue is not the centre's to move");
                seen.push((wheel.hsv.s, wheel.hsv.v));
            }
            // Let go out on the ring: still the centre's, which is the same bug
            // the other way round and what the recorded aim exists for.
            let aimed = wheel.release(on_the_ring(210.0, 0.5));
            assert!(matches!(aimed, WheelAim::Centre(_)), "{aimed:?}");
            assert_eq!(wheel.hsv.h, 123.0, "the release moved the hue");
            // It slid rather than froze: a drag right round the outside of the
            // shape reaches distinct places, not one.
            seen.dedup();
            assert!(seen.len() > 4, "{shape:?} froze: {seen:?}");
        }
    }

    /// A press in a gap between the shape and the ring moves nothing at all.
    /// Nothing is drawn there, and a press that quietly snapped the marker to
    /// the nearest edge is the control that lies.
    #[test]
    fn a_press_between_the_shape_and_the_ring_moves_no_colour() {
        // Straight down from the centre of an apex-up triangle is the middle of
        // the opposite edge, which is the widest of the three gaps.
        let hub = hub_at(WheelShape::Triangle, 0.0);
        let from = CENTRE + vec2(0.0, INNER * 0.7);
        assert!(!hub.contains(from), "that is not a gap");
        assert!(
            (from - CENTRE).length() < INNER * RING_GRIP,
            "nor is it ring"
        );
        assert_eq!(settle(CENTRE, INNER, OUTER, hub, from), Aim::Nothing);

        // Held still rather than following the hue, so the wheel's own shape is
        // the one `hub_at` just measured — with the hue at 200° and the apex
        // chasing it, this point would be *inside* the triangle and rightly
        // taken by it.
        let before = Hsv::new(200.0, 0.4, 0.6);
        let mut wheel = Wheel::new(WheelShape::Triangle, false, before);
        assert_eq!(wheel.press(from), WheelAim::Idle);
        // And a drag begun there stays nothing, wherever it goes.
        assert_eq!(wheel.drag(from, CENTRE), WheelAim::Idle);
        assert_eq!(wheel.drag(from, on_the_ring(90.0, 0.5)), WheelAim::Idle);
        assert_eq!(wheel.release(CENTRE), WheelAim::Idle);
        assert_eq!(
            (wheel.hsv.h, wheel.hsv.s, wheel.hsv.v),
            (before.h, before.s, before.v)
        );
    }

    /// The picker driven the way a hand drives it: real pointer events through
    /// a real [`egui::Context`], calling [`show`] itself.
    ///
    /// Every other test here drives [`resolve`], which is the same dispatch the
    /// picker runs but not the same *frame timing*. Whether
    /// `interact_pointer_pos` is `Some` on the press frame, whether egui clears
    /// `press_origin` on the release, and whether `primary_pressed` is raised
    /// on exactly one frame were all read out of egui's source and, until this,
    /// never executed. [`wheel_aim`] — that reading, and the memory the settled
    /// aim is kept in — had no test over it at all.
    ///
    /// The property is geometry-free, which is what lets it be asserted without
    /// a second copy of where the ring is: **one gesture may move the hue, or
    /// the saturation and value, or neither — never both.** The ring is one
    /// control and the centre is the other, and a gesture belongs to one of
    /// them for its whole life. All three causes of the reported bug break
    /// exactly this: the press frame handed a ring press to the field, the
    /// release frame did it again, and re-settling mid-gesture let the triangle
    /// take a drag the ring had started.
    #[test]
    fn a_real_gesture_moves_one_control_and_not_both() {
        use crate::theme::{Palette, ThemeKind, metrics};
        use egui::{Event, Modifiers, PointerButton, RawInput, Rect};

        struct Picker {
            shape: WheelShape,
            rotate: bool,
            mirrored: bool,
            angles: WheelAngles,
            harmony: Harmony,
            hsv: Hsv,
        }

        let ctx = egui::Context::default();
        let palette = Palette::of(ThemeKind::Graphite);
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(metrics::PANEL, 600.0));

        let run = |picker: &mut Picker, events: Vec<Event>| {
            let input = RawInput {
                screen_rect: Some(screen),
                events,
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| {
                show(
                    ui,
                    &palette,
                    PickerMode::Wheel,
                    &mut picker.shape,
                    &mut picker.rotate,
                    &mut picker.mirrored,
                    &mut picker.angles,
                    &mut picker.harmony,
                    &mut picker.hsv,
                );
            });
        };

        let button = |pos: Pos2, pressed: bool| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::default(),
        };

        // A grid over the whole panel: some of these land on the ring, some in
        // the centre, some in the gaps and some on the controls below. Which is
        // which is deliberately not worked out here — the property holds for
        // every one of them.
        let mut probed = 0;
        let mut hue_gestures = 0;
        let mut sv_gestures = 0;
        for row in 0..12 {
            for column in 0..12 {
                let spot = pos2(
                    screen.left() + (column as f32 + 0.5) * screen.width() / 12.0,
                    screen.top() + (row as f32 + 0.5) * 200.0 / 12.0,
                );
                // "Rotate with hue" on, which is the shipped default and the
                // case where the shape moves under the gesture.
                let mut picker = Picker {
                    shape: WheelShape::Triangle,
                    rotate: true,
                    mirrored: false,
                    angles: WheelAngles::default(),
                    harmony: Harmony::Complementary,
                    hsv: Hsv::new(210.0, 0.4, 0.6),
                };
                // A frame with the pointer merely present, so the widget exists
                // for egui's hit test before anything is pressed.
                run(&mut picker, vec![Event::PointerMoved(spot)]);
                let before = picker.hsv;

                let mut hue_moved = false;
                let mut sv_moved = false;
                let mut check = |picker: &Picker, what: &str| {
                    hue_moved |= picker.hsv.h != before.h;
                    sv_moved |= (picker.hsv.s, picker.hsv.v) != (before.s, before.v);
                    assert!(
                        !(hue_moved && sv_moved),
                        "one gesture from {spot:?} moved the hue *and* the \
                         marker, by {what}: {before:?} -> {:?}",
                        picker.hsv
                    );
                    (hue_moved, sv_moved)
                };

                run(&mut picker, vec![button(spot, true)]);
                check(&picker, "the press");
                // Far enough to be a drag by egui's own reckoning, and across
                // the middle, which is where a re-settled aim changes hands.
                for step in 1..=6 {
                    let to = spot + vec2(step as f32 * 7.0, step as f32 * 5.0);
                    run(&mut picker, vec![Event::PointerMoved(to)]);
                    check(&picker, "a drag frame");
                }
                let last = spot + vec2(42.0, 30.0);
                run(&mut picker, vec![button(last, false)]);
                check(&picker, "the release");
                run(&mut picker, Vec::new());
                let (hue, sv) = check(&picker, "the frame after");

                probed += 1;
                hue_gestures += usize::from(hue);
                sv_gestures += usize::from(sv);
            }
        }
        assert_eq!(probed, 144);
        // And the sweep reaches both controls, so the property above is about
        // a picker that answers the pointer rather than one that ignores it.
        // Counted rather than aimed: working out where the ring is would be a
        // second copy of the geometry `Hub` exists to be the only statement of.
        assert!(hue_gestures > 0, "no gesture reached the hue ring");
        assert!(sv_gestures > 0, "no gesture reached the centre");
    }

    /// Swapping exchanges white and black and moves nothing else.
    ///
    /// Both halves matter and only together. That the two corners change places
    /// is the feature; that the *outline* does not move is what makes the
    /// setting free — the hit test, the skirt and the ring's clearance are all
    /// derived from these three points, so a swap that also turned the shape
    /// would need every one of them re-checked.
    #[test]
    fn swapping_exchanges_white_and_black_and_moves_nothing_else() {
        for degrees in [0.0_f32, 17.0, 90.0, 233.0, 359.0] {
            let base = degrees.to_radians();
            let (hue, white, black) = triangle_corners(CENTRE, RADIUS, base, false);
            let (hue_s, white_s, black_s) = triangle_corners(CENTRE, RADIUS, base, true);

            assert!(
                apart(hue, hue_s) < 1e-3,
                "the hue corner moved at {degrees}"
            );
            assert!(
                apart(white_s, black) < 1e-3,
                "white did not land where black was, at {degrees}"
            );
            assert!(
                apart(black_s, white) < 1e-3,
                "black did not land where white was, at {degrees}"
            );
        }
    }

    /// **No angle whatever reaches the swapped arrangement, and the two
    /// controls together reach all six** — which is the whole reason this is a
    /// second control rather than two more stops on the Angle rail.
    ///
    /// The rail turns all three corners together, so it walks the three
    /// *rotations* of the corner labelling; the swap is a reflection and gives
    /// the other three. Somebody proposing to fold one into the other will find
    /// this test, so it has to check both halves of the sentence rather than
    /// only the first: a sweep against one swapped triangle says the rail
    /// cannot get there, and the six-way comparison says the pair does not
    /// waste a control by reaching the same arrangement twice.
    #[test]
    fn the_angle_and_the_swap_reach_six_arrangements_and_no_angle_reaches_a_swap() {
        // Half a degree at a time round the whole turn, which is finer than the
        // 45° the rail snaps to and finer than anything that could be typed.
        let swapped = triangle_corners(CENTRE, RADIUS, 0.0, true);
        for step in 0..720 {
            let base = (step as f32 * 0.5).to_radians();
            let plain = triangle_corners(CENTRE, RADIUS, base, false);
            let same = apart(plain.0, swapped.0) < 0.5
                && apart(plain.1, swapped.1) < 0.5
                && apart(plain.2, swapped.2) < 0.5;
            assert!(!same, "{}° reproduces the swap", step as f32 * 0.5);
        }

        // And the six are six. A third of a turn is the triangle's own period,
        // so these are every arrangement of the three corner roles that either
        // control can produce; any two of them being the same set of three
        // labelled points would mean one of the six is unreachable and a
        // control is doing less than it says.
        let all: Vec<(Pos2, Pos2, Pos2)> = [false, true]
            .into_iter()
            .flat_map(|mirrored| {
                [0.0_f32, 120.0, 240.0]
                    .into_iter()
                    .map(move |d| triangle_corners(CENTRE, RADIUS, d.to_radians(), mirrored))
            })
            .collect();
        assert_eq!(all.len(), 6);
        for (i, a) in all.iter().enumerate() {
            for (j, b) in all.iter().enumerate().skip(i + 1) {
                let same = apart(a.0, b.0) < 0.5 && apart(a.1, b.1) < 0.5 && apart(a.2, b.2) < 0.5;
                assert!(!same, "arrangements {i} and {j} are the same one");
            }
        }
    }

    /// The setting reaches the canvas, and not only [`triangle_corners`].
    ///
    /// A guard on a model is not a guard on the panel: every test above passes
    /// with `wheel` never handing the setting to `hub_of` at all. So this
    /// drives the shipped [`show`], and it starts from the case where the two
    /// readings disagree, which is the only kind of test of a two-state reading
    /// that is worth anything.
    ///
    /// **It covers one of `wheel`'s two `hub_of` calls, and that is measured
    /// rather than claimed.** Mutating the one that *paints the shape and reads
    /// the drag* fails this test; mutating the one that *judges the press*
    /// leaves the whole suite green — because the swap is a reflection about
    /// the axis through the hue corner, so both hubs hold the same three points
    /// and [`Hub::contains`] cannot tell them apart. That call site is
    /// therefore correct-by-construction rather than guarded, and the comment
    /// there says so.
    ///
    /// A sweep rather than an aimed press at the white corner: working out
    /// where that corner lands on the panel would be a second copy of the
    /// geometry [`Hub`] exists to be the only statement of. What is asserted is
    /// that *some* press on the wheel reads a different colour with the corners
    /// swapped, which is exactly what "the setting reaches the shape" means.
    ///
    /// Presses stay inside the wheel's own square, and the flag is checked
    /// afterwards: the toggle that sets it is a few points below, and a sweep
    /// that clicked its own control would prove nothing about the triangle.
    #[test]
    fn swapping_white_and_black_actually_changes_the_colour_a_press_reads() {
        use crate::theme::{Palette, ThemeKind, metrics};
        use egui::{Event, Modifiers, PointerButton, RawInput, Rect};

        let ctx = egui::Context::default();
        let palette = Palette::of(ThemeKind::Graphite);
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(metrics::PANEL, 600.0));

        // One click on the wheel, with the swap set either way, answering the
        // colour it left behind. The angle is held still — "Rotate with hue" is
        // off — so the only thing that can differ between the two runs is which
        // corner is white.
        let click = |mirrored: bool, at: Pos2| {
            let mut shape = WheelShape::Triangle;
            let mut rotate = false;
            let mut swap = mirrored;
            let mut angles = WheelAngles::default();
            let mut harmony = Harmony::Complementary;
            let mut hsv = Hsv::new(210.0, 0.4, 0.6);
            let mut frame = |events: Vec<Event>| {
                let _ = ctx.run_ui(
                    RawInput {
                        screen_rect: Some(screen),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        show(
                            ui,
                            &palette,
                            PickerMode::Wheel,
                            &mut shape,
                            &mut rotate,
                            &mut swap,
                            &mut angles,
                            &mut harmony,
                            &mut hsv,
                        );
                    },
                );
            };
            frame(vec![Event::PointerMoved(at)]);
            for pressed in [true, false] {
                frame(vec![Event::PointerButton {
                    pos: at,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: Modifiers::default(),
                }]);
            }
            (hsv, swap)
        };

        let mut disagreed = 0;
        for row in 0..10 {
            for column in 0..10 {
                let at = pos2(
                    screen.left() + (column as f32 + 0.5) * screen.width() / 10.0,
                    screen.top() + (row as f32 + 0.5) * 170.0 / 10.0,
                );
                let (plain, plain_flag) = click(false, at);
                let (swapped, swapped_flag) = click(true, at);
                assert!(!plain_flag && swapped_flag, "a press moved the setting");
                if (plain.s, plain.v) != (swapped.s, swapped.v) {
                    disagreed += 1;
                }
            }
        }
        assert!(
            disagreed > 0,
            "the swap changed no colour anywhere on the wheel: the setting is \
             not reaching the shape the drag is read from"
        );
    }

    /// A gesture is settled at its press and not at its release.
    ///
    /// egui reports a *drag* only once the pointer has travelled far enough to
    /// be one, which a press is not yet and a release is no longer — and it
    /// clears the press origin on the very frame a click is reported. Reading
    /// the pointer's own position on those two frames is what handed a press on
    /// the hue ring to the saturation and value field.
    #[test]
    fn a_gesture_is_settled_at_its_press_and_not_at_its_release() {
        let press = pos2(10.0, 10.0);
        let now = pos2(80.0, 90.0);

        // The press: nothing recorded yet, so it is settled here.
        assert_eq!(
            frame(
                Reported {
                    at: Some(press),
                    press_origin: Some(press),
                    pressed: true,
                },
                None
            ),
            Frame::Press(press, press)
        );
        // Held down since, whether or not egui calls it a drag yet: the aim
        // stands and only the position moves.
        assert_eq!(
            frame(
                Reported {
                    at: Some(now),
                    press_origin: Some(press),
                    pressed: false,
                },
                Some(Aim::Ring)
            ),
            Frame::Held(Aim::Ring, now)
        );
        // Released: egui has cleared its own origin, and what was recorded is
        // all that is left.
        assert_eq!(
            frame(
                Reported {
                    at: Some(now),
                    press_origin: None,
                    pressed: false,
                },
                Some(Aim::Ring)
            ),
            Frame::Held(Aim::Ring, now)
        );
        // Pressed and released inside one frame — a click too fast to be seen
        // twice. Nothing recorded, and the pointer has had no frame in which to
        // move, so its own position is the origin.
        assert_eq!(
            frame(
                Reported {
                    at: Some(now),
                    press_origin: None,
                    pressed: true,
                },
                None
            ),
            Frame::Press(now, now)
        );
        // The primary button going down is a new gesture whatever the position
        // — including one that lands on the exact pixel an abandoned gesture
        // was pressed at, which a comparison of origins read as the same
        // gesture and let inherit its aim.
        assert_eq!(
            frame(
                Reported {
                    at: Some(press),
                    press_origin: Some(press),
                    pressed: true,
                },
                Some(Aim::Centre)
            ),
            Frame::Press(press, press)
        );
        // No gesture on this widget: a pointer merely passing over the wheel,
        // or one pressed on something else. The record goes.
        assert_eq!(
            frame(
                Reported {
                    at: None,
                    press_origin: Some(press),
                    pressed: false,
                },
                Some(Aim::Ring)
            ),
            Frame::Idle
        );
        assert_eq!(frame(Reported::default(), None), Frame::Idle);
    }

    /// A second button pressed part-way through a drag must change nothing.
    ///
    /// egui keeps this widget's interaction — `potential_drag_id` is only
    /// assigned when it is `None` — so `at` goes on being reported, while
    /// `press_origin` jumps to wherever the pointer now is and is then cleared
    /// by that button's release. Reading either as "a new gesture began"
    /// re-settled the aim against the live hub: a ring drag whose pointer had
    /// crossed the triangle threw saturation and value at the pointer, twice.
    /// A resting hand on a tablet is the ordinary way to meet this, which is why
    /// it is not a remote case.
    #[test]
    fn a_second_button_pressed_mid_drag_changes_nothing() {
        for shape in WheelShape::ALL {
            let start = Hsv::new(200.0, 0.42, 0.58);
            let mut wheel = Wheel::new(shape, true, start);
            let press = on_the_ring(40.0, 0.5);
            assert!(matches!(wheel.press(press), WheelAim::Ring(_)));
            // Drag until the pointer is over the middle, which is where the
            // second button does the damage.
            assert!(matches!(
                wheel.drag(press, on_the_ring(80.0, 0.5)),
                WheelAim::Ring(_)
            ));
            let hue = wheel.hsv.h;

            let over_the_centre = CENTRE;
            for (what, aimed) in [
                (
                    "the second press",
                    wheel.second_button_down(over_the_centre),
                ),
                ("the frame after", wheel.drag(press, over_the_centre)),
                (
                    "the second release",
                    wheel.second_button_up(over_the_centre),
                ),
            ] {
                assert!(
                    matches!(aimed, WheelAim::Ring(_)),
                    "{shape:?}: {what} took the drag as {aimed:?}"
                );
            }
            assert_eq!(
                (wheel.hsv.s, wheel.hsv.v),
                (start.s, start.v),
                "{shape:?}: the marker moved"
            );
            // The hue is still the ring's to move, and moved only because the
            // pointer did — to the centre, where `ring_hue` reads zero.
            assert_eq!(wheel.hsv.h, ring_hue(CENTRE, over_the_centre));
            assert!(hue != wheel.hsv.h || hue == 0.0);
        }
    }

    /// A record left behind by a gesture that ended with no frame to clear it
    /// must not be inherited — not even by a press at the very pixel it was
    /// settled at, which is an ordinary thing to do at a scale factor of one.
    #[test]
    fn a_new_press_on_the_same_pixel_settles_afresh() {
        let spot = on_the_ring(90.0, 0.5);
        let mut wheel = Wheel::new(WheelShape::Triangle, true, Hsv::new(10.0, 0.5, 0.5));
        // A gesture on the ring, abandoned without a release — the picker mode
        // was switched, so `wheel` was not drawn again to clear the record.
        assert!(matches!(wheel.press(spot), WheelAim::Ring(_)));
        wheel.held = Some(Aim::Centre);

        // Back on the wheel, pressing the same pixel. The stale aim would have
        // made this a press on the saturation and value shape.
        assert!(
            matches!(wheel.press(spot), WheelAim::Ring(_)),
            "the stale aim was inherited"
        );
        // And an idle frame is what clears a record in the ordinary case.
        assert_eq!(wheel.idle(), WheelAim::Idle);
        assert_eq!(wheel.held, None);
    }

    /// The release frame of a drag out of the centre must not be handed to the
    /// ring. This is the same bug the other way round, and it is what the
    /// recorded origin exists for: without it the release falls back to the
    /// pointer, which by then is out on the ring.
    #[test]
    fn a_drag_released_over_the_ring_does_not_move_the_hue() {
        let mut wheel = Wheel::new(WheelShape::Triangle, true, Hsv::new(30.0, 0.5, 0.5));
        assert!(matches!(wheel.press(CENTRE), WheelAim::Centre(_)));
        let release = on_the_ring(210.0, 0.5);
        let aimed = wheel.release(release);
        assert!(
            matches!(aimed, WheelAim::Centre(_)),
            "the release was read as a press on the ring: {aimed:?}"
        );
        assert_eq!(wheel.hsv.h, 30.0);
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
    ///
    /// Over both swap states as well as several angles: swapping reverses the
    /// order the three corners come back in, so the edges the loop below walks
    /// are traversed the other way round and `skirt_corner`'s outward direction
    /// is the one thing that could have depended on the winding.
    #[test]
    fn the_triangles_skirt_is_one_feather_wide_along_every_edge() {
        let f = 1.0;
        for (degrees, mirrored) in [0.0_f32, 17.0, 45.0, 120.0, 271.0, 359.0]
            .into_iter()
            .flat_map(|d| [(d, false), (d, true)])
        {
            let (a, b, c) = triangle_corners(CENTRE, RADIUS, degrees.to_radians(), mirrored);
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
                        "edge {i}-{j} at {degrees}° (mirrored {mirrored}) is \
                         feathered over {across}, not {f}"
                    );
                }
                // And outside, not folded back over the gradient.
                let out = outer[i] - centroid;
                let inn = inner[i] - centroid;
                assert!(
                    out.length() > inn.length(),
                    "corner {i} folded inwards (mirrored {mirrored})"
                );
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
    /// A seventh shot is taken at **zero saturation**, which is the case the
    /// markers' own comment argues from and the one that cannot be reasoned
    /// about — the swatch row is six identical greys there, so the ring is the
    /// only thing left saying which member is the colour in hand. It is written
    /// in each of the six themes rather than only in Graphite, because the
    /// relation trigger's outline is `Palette::border` and a border invisible
    /// against `Palette::dock` would put the picker back where it started.
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

        let field = vec2(metrics::PANEL - 2.0 * metrics::PANEL_PAD as f32, 300.0);
        let mut shot = |name: String, palette: &Palette, relation: Harmony, hsv: Hsv| {
            let mut harmony = relation;
            let mut hsv = hsv;
            let image = stage.shoot(field, 2.0, palette, palette.dock, |root| {
                harmony_wheel(root, palette, &mut harmony, &mut hsv);
            });
            docshot::write_png(&dir.join(format!("{name}.png")), &image).expect("write the png");
        };

        let graphite = Palette::of(crate::theme::ThemeKind::Graphite);
        for (index, relation) in Harmony::ALL.into_iter().enumerate() {
            let name = format!("{}-{}", index + 1, relation.label().replace(' ', "-"));
            shot(name, &graphite, relation, Hsv::new(28.0, 0.72, 0.86));
        }
        // The grey case, in every theme: the swatch row says nothing here, so
        // whatever tells the base from the rest has to be on the ring.
        for kind in crate::theme::ThemeKind::ALL {
            let name = format!("grey-{}", kind.id());
            shot(
                name,
                &Palette::of(kind),
                Harmony::Tetrad,
                Hsv::new(28.0, 0.0, 0.7),
            );
        }
        println!("wrote the shots to {}", dir.display());
    }

    /// The base's second ring is on the hue band, not hanging off it.
    ///
    /// [`HARMONY_BASE_GAP`] is the one number in this picker that is chosen
    /// against a *margin* rather than against a look, so it needs a guard that
    /// measures the margin. The sweep is over every side the wheel can be drawn
    /// at, through [`ring_radii`], which is the same function the two wheels
    /// call — recomputing `size * 0.5` here would be a test that agrees with
    /// whatever the arithmetic becomes.
    ///
    /// Both halves of what the constant's docs claim are checked, because a
    /// guard that only asserts the comfortable half is one that will be read as
    /// promising the other: it fits from 54 points up, and it overhangs by no
    /// more than a point below that. An assertion that it *always* fits would be
    /// false today, and one that never checked the floor would go quiet the day
    /// somebody widened the gap.
    #[test]
    fn the_base_ring_stays_on_the_hue_band() {
        // The outermost thing drawn for the base: the second ring, plus the half
        // of its stroke that lies outside its own radius.
        let reach = HARMONY_MARKER + HARMONY_BASE_GAP + MARKER_STROKE.width * 0.5;
        let mut sides = vec![MIN_PICKER, 53.0, 54.0, 176.0];
        sides.extend((48..=176).map(|n| n as f32));
        for size in sides {
            let (inner, outer) = ring_radii(size);
            // A marker sits on the middle of the band, so what it has to spare
            // is half the band's width.
            let half_band = (outer - inner) * 0.5;
            if size >= 54.0 {
                assert!(
                    reach <= half_band + 1e-3,
                    "at {size} the base ring reaches {reach} into a half-band of {half_band}",
                );
            } else {
                assert!(
                    reach <= half_band + 1.0 + 1e-3,
                    "at {size} the base ring overhangs by {}, which is more than the \
                     point `HARMONY_BASE_GAP`'s docs admit to",
                    reach - half_band,
                );
            }
        }
    }

    /// No marker on the harmony wheel is filled, and the base is the one
    /// wearing two rings.
    ///
    /// This is the artist's complaint pinned rather than paraphrased: a member
    /// used to be a *filled* disc of its own colour at the current saturation
    /// and value, painted over the vivid hue it was pointing at. So the
    /// assertion is about the fill — `Color32::TRANSPARENT` on every circle this
    /// pass draws — and not about which colour was chosen, because "it drew the
    /// right muddy orange" is the defect passing its own test.
    ///
    /// The counts are the other half. Every member has to wear the *same* mark,
    /// which is `HARMONY_MARKER` exactly `hues.len()` times, and exactly one
    /// second ring says which one is in hand. Both directions matter: dropping
    /// the second ring leaves nothing saying which member is the colour in hand,
    /// which is what the fill used to say and is the thing this change had to
    /// replace rather than remove.
    #[test]
    fn every_harmony_marker_is_an_open_ring_and_only_the_base_wears_two() {
        use crate::theme::metrics;

        fn circles(shape: &egui::Shape, into: &mut Vec<egui::epaint::CircleShape>) {
            match shape {
                egui::Shape::Circle(c) => into.push(*c),
                egui::Shape::Vec(shapes) => {
                    for s in shapes {
                        circles(s, into);
                    }
                }
                _ => {}
            }
        }

        let p = Palette::of(crate::theme::ThemeKind::Graphite);
        let field = vec2(metrics::PANEL - 2.0 * metrics::PANEL_PAD as f32, 300.0);
        for relation in Harmony::ALL {
            let ctx = egui::Context::default();
            let mut harmony = relation;
            let mut hsv = Hsv::new(28.0, 0.72, 0.86);
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), field)),
                    ..Default::default()
                },
                |ui| {
                    harmony_wheel(ui, &p, &mut harmony, &mut hsv);
                },
            );
            let mut seen = Vec::new();
            for clipped in &output.shapes {
                circles(&clipped.shape, &mut seen);
            }

            for circle in &seen {
                assert_eq!(
                    circle.fill,
                    Color32::TRANSPARENT,
                    "{relation:?}: a marker at radius {} is filled with {:?}",
                    circle.radius,
                    circle.fill,
                );
            }
            let members = relation.hues(hsv.h).as_slice().len();
            let at = |r: f32| seen.iter().filter(|c| c.radius == r).count();
            assert_eq!(
                at(HARMONY_MARKER),
                members,
                "{relation:?}: {} members but {} markers at the shared radius",
                members,
                at(HARMONY_MARKER),
            );
            assert_eq!(
                at(HARMONY_MARKER + HARMONY_BASE_GAP),
                1,
                "{relation:?}: {} second rings, so nothing or more than one thing \
                 claims to be the colour in hand",
                at(HARMONY_MARKER + HARMONY_BASE_GAP),
            );
        }
    }

    /// The relation picker is *drawn* outlined, in every theme.
    ///
    /// **A guard on the widget is not a guard on the panel**, and this whole
    /// defect is that lesson twice over: `widgets`'
    /// `an_outlined_trigger_draws_a_line_that_reads_on_its_surface` measures
    /// what `Dropdown::outlined` produces and cannot see whether anything asks
    /// for it — drop the one call below and every ratio it checks still passes.
    /// So this measures the ink that reached the pass, off the whole
    /// `harmony_wheel`, which is where a revert would land.
    ///
    /// It names the exact derived colour rather than asking for "some ink that
    /// reads", because this pass draws a hue ring: nearly every vivid hue on it
    /// clears 3:1 against a panel, so the weaker question would be answered by
    /// the wheel and would hold whatever the trigger did.
    #[test]
    fn the_relation_picker_is_drawn_outlined() {
        use crate::theme::contrast::{self, Ink};
        use crate::theme::{ThemeKind, metrics};
        use crate::widgets::tests::inks_drawn;

        for kind in ThemeKind::ALL {
            let ctx = egui::Context::default();
            let q = Palette::of(kind);
            let want = contrast::ink_on(q.dock, Ink::Dim);
            let field = vec2(metrics::PANEL - 2.0 * metrics::PANEL_PAD as f32, 300.0);
            // Twice, for the font atlas: the label is what a first pass has no
            // glyphs for, and the outline is drawn beside it either way.
            let mut seen = Vec::new();
            for _ in 0..2 {
                let mut harmony = Harmony::Tetrad;
                let mut hsv = Hsv::new(28.0, 0.72, 0.86);
                seen = inks_drawn(&ctx, field, |ui| {
                    harmony_wheel(ui, &q, &mut harmony, &mut hsv);
                });
            }
            assert!(
                seen.contains(&want),
                "{kind:?}: the relation picker drew no {want:?} — the outline \
                 derived from this theme's own panel surface",
            );
        }
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
