//! Moving, scaling and rotating a rectangle of the document.
//!
//! # What a transform *is*
//!
//! A [`Transform`] is a source rectangle in document space and three numbers
//! applied to it — an offset, a scale and an angle — with the **centre of the
//! source rectangle** as the pivot for the last two. It is not a general 3×2
//! matrix that the interface pokes at: the handles have to be able to read the
//! current scale and angle back out to draw themselves, and a matrix that has
//! been multiplied into cannot say which part of it was the rotation.
//!
//! [`Transform::matrix`] is the composition, and [`Transform::inverse`] is what
//! the GPU actually wants. A resampler walks the *destination* and asks where
//! each output pixel came from, so the forward map is only ever used for
//! geometry: where the corners are, where the handles are, and which rectangle
//! of the canvas the result can reach.
//!
//! # Filtering
//!
//! **Bilinear, and it is the hardware sampler's** — the source pixels live in
//! the layer texture array, which is filtered `Linear` already, so
//! `transform.wgsl` samples with the inverse map and the interpolation is free.
//! There is deliberately no CPU resampler here to go with it: a second
//! implementation of the same filter, called by nothing, is exactly the drift
//! this project refuses everywhere else (see `composite.wgsl` and
//! `commit.wgsl`). What this module owes the shader is the *map*, and
//! `a_transform_and_its_inverse_are_exact_opposites` is what pins it.
//!
//! Bilinear rather than nearest or bicubic, and both directions matter. Nearest
//! makes a rotation a staircase and a scale a grid of doubled pixels, which is
//! the one artefact a painter cannot work around. Bicubic is sharper on a
//! downscale and rings on an upscale — it overshoots at a hard edge, which on
//! premultiplied colour shows up as a dark halo — and it is four times the taps
//! for a filter the sampler does not implement, so it would have to be written
//! out per fragment. Bilinear is what every paint application uses for a live
//! transform preview, and a live preview is what this is.
//!
//! # What is not here
//!
//! No perspective, no free distort, and no numeric entry. Each is a real
//! feature and none is drawn in the interface.
//!
//! A **flip** is here, and is nothing more than a negative [`Transform::scale`]
//! — see [`Transform::MIN_SCALE`] for why that used to be refused and is not
//! any more.

use crate::geom::{PixelRect, Rect};
use glam::{Mat2, UVec2, Vec2};

/// How far outside the transformed quad the damaged rectangle reaches, in
/// document pixels.
///
/// Bilinear filtering reads a half-texel skirt around every sample, and the
/// quad's own edge is antialiased against transparency. One pixel covers both,
/// and the failure direction is the one the dab pass already learned the hard
/// way: a rectangle too tight leaves the edge of the mark uncommitted.
const SKIRT: f32 = 1.0;

/// A move, scale and rotation of one rectangle of the document.
///
/// `Copy`, small, and free of any GPU or interface type: the same value drives
/// the shader's inverse map, the handles the pointer is tested against, and the
/// rectangle the undo patch spans.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    /// Where the floating pixels sit at identity, in document space. The pivot
    /// for the scale and the rotation is its centre.
    source: Rect,
    /// Document pixels the source has been moved by.
    pub offset: Vec2,
    /// Scale about the pivot, per axis. **Negative is a flip**, and is reached
    /// both by dragging a handle past the side opposite it and by
    /// [`Transform::flip_x`] / [`Transform::flip_y`] — see
    /// [`Transform::MIN_SCALE`].
    pub scale: Vec2,
    /// Rotation about the pivot, radians, clockwise on screen (document y is
    /// down).
    pub angle: f32,
}

/// Which part of the transform a drag is moving.
///
/// The eight box handles are named by the local corner they sit on, as a
/// `(x, y)` pair in `-1..=1` — which is also exactly what the scale maths
/// needs, so there is no table mapping one to the other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    /// Drag the floating pixels bodily.
    Move,
    /// Drag one handle of the box. `local` is the corner it sits on: `(-1, -1)`
    /// is top-left, `(0, -1)` the middle of the top edge, and so on. A zero
    /// component means that axis is not scaled, which is what makes an edge
    /// handle one-dimensional without a second variant.
    Scale { local: (i8, i8) },
    /// Turn about the centre. Reached **anywhere outside the box** that is not
    /// on one of its handles.
    ///
    /// A ring just outside the four corners was tried first and is what most
    /// applications draw, but the ring is invisible: nothing marks where it
    /// begins or ends, so the whole gesture had to be discovered by waving the
    /// pointer about near a corner. Outside-is-rotate needs no target at all,
    /// and the only thing it takes away is "click away from the box to put it
    /// down" — which is the pointer layer's to keep, by telling a click from a
    /// drag. See `app.rs`.
    Rotate,
}

impl Handle {
    /// The eight box handles, corners first.
    ///
    /// Corners before edges because the hit test walks this in order and a
    /// corner must win where the two overlap: a corner handle is the one that
    /// scales both axes, and losing it to the edge beside it would make a box
    /// impossible to scale freely at small sizes.
    pub const BOX: [Handle; 8] = [
        Handle::Scale { local: (-1, -1) },
        Handle::Scale { local: (1, -1) },
        Handle::Scale { local: (1, 1) },
        Handle::Scale { local: (-1, 1) },
        Handle::Scale { local: (0, -1) },
        Handle::Scale { local: (1, 0) },
        Handle::Scale { local: (0, 1) },
        Handle::Scale { local: (-1, 0) },
    ];

    fn local(self) -> Option<Vec2> {
        match self {
            Handle::Scale { local } => Some(Vec2::new(local.0 as f32, local.1 as f32)),
            _ => None,
        }
    }
}

impl Transform {
    /// The smallest scale a drag can reach, on either axis — as a
    /// **magnitude**. The sign is the drag's.
    ///
    /// What has to be kept away from is *zero*, and only zero: a zero-scale
    /// transform has no inverse, so the shader's map would be a division by
    /// zero and the destination rectangle would collapse to a point.
    ///
    /// A negative scale used to be refused here as well, on the grounds that a
    /// flip is a feature with its own controls rather than something a hand
    /// should stumble into. That was wrong twice. Dragging the right edge past
    /// the left is what every other application means by "flip it", so refusing
    /// it makes the box stick against an invisible wall for no stated reason;
    /// and there is nothing downstream that a negative scale breaks — the
    /// matrix stays invertible (the determinant only changes sign), `quad`'s
    /// corners stay in a cycle because an affine map takes adjacent corners to
    /// adjacent corners, and the shader reads the inverse map without ever
    /// asking which way round it is. So the magnitude is clamped and the sign
    /// follows the hand.
    pub const MIN_SCALE: f32 = 0.01;

    /// A transform that has not moved anything yet.
    pub fn identity(source: PixelRect) -> Self {
        Self {
            source: Rect::new(
                Vec2::new(source.x as f32, source.y as f32),
                Vec2::new(
                    (source.x + source.width) as f32,
                    (source.y + source.height) as f32,
                ),
            ),
            offset: Vec2::ZERO,
            scale: Vec2::ONE,
            angle: 0.0,
        }
    }

    /// The rectangle the floating pixels occupy at identity.
    pub fn source(&self) -> Rect {
        self.source
    }

    /// Put a **different** rectangle under the same transform, leaving every
    /// point exactly where the old one put it.
    ///
    /// The source rectangle's centre is the pivot for the scale and the
    /// rotation, so growing it — which is what typing another word into a text
    /// float does — moves the pivot, and moving a pivot moves everything that
    /// was already on screen. `Transform::identity(new_rect)` is the obvious
    /// repair and throws away every drag the artist has made.
    ///
    /// The compensation is one line and it is **exact** rather than close.
    /// With `m = R·S`, `apply(p) = m·(p − pivot) + pivot + offset`; moving the
    /// pivot from `p₀` to `q` and setting `offset' = offset + (m − I)·(q − p₀)`
    /// makes the two expressions identical term for term, so no glyph already
    /// placed moves by a millionth of a pixel and the new ones land in identity
    /// space and are carried by the same matrix.
    ///
    /// At identity `m = I`, so the correction is exactly zero and
    /// [`Self::is_identity`] still reads true — which is what stops a text
    /// float that was typed into but never dragged from recording an edit it
    /// did not make.
    ///
    /// **Nothing calls this yet**, and that is said here rather than left to be
    /// discovered. Text placed on the canvas today is a *paste* — pixels the
    /// moment they land, with no caret and therefore no box that grows as it is
    /// typed into. This is here because the exactness is the part that is worth
    /// proving before anything depends on it, and it is provable without a
    /// device: the pair of tests beside
    /// `a_transform_and_its_inverse_are_exact_opposites` is the whole of the
    /// argument, and it will not have to be reconstructed by whoever builds the
    /// caret. See `docs/text-tool.md` §4(a).
    pub fn reseat(&mut self, source: PixelRect) {
        let was = self.pivot();
        let m = self.rotation() * Mat2::from_diagonal(self.scale);
        self.source = Rect::new(
            Vec2::new(source.x as f32, source.y as f32),
            Vec2::new(
                (source.x + source.width) as f32,
                (source.y + source.height) as f32,
            ),
        );
        self.offset += (m - Mat2::IDENTITY) * (self.pivot() - was);
    }

    /// True when this transform would leave every pixel exactly where it is.
    ///
    /// What decides whether a gesture is worth committing at all: a click that
    /// picked the tool up and put it down again must not produce an undo entry
    /// naming an edit that changed nothing.
    ///
    /// A flip is not the identity even though the box has not moved: `scale` is
    /// `(-1, 1)` rather than `(1, 1)`, and the pixels inside the box are
    /// genuinely different ones. Falling out of the comparison is the point —
    /// nothing here had to learn what a flip is.
    pub fn is_identity(&self) -> bool {
        self.offset == Vec2::ZERO && self.scale == Vec2::ONE && self.angle == 0.0
    }

    /// The pivot — the centre of the source rectangle, before any movement.
    pub fn pivot(&self) -> Vec2 {
        (self.source.min + self.source.max) * 0.5
    }

    /// Half the source rectangle's width and height.
    fn half(&self) -> Vec2 {
        (self.source.max - self.source.min) * 0.5
    }

    /// The rotation on its own.
    fn rotation(&self) -> Mat2 {
        Mat2::from_angle(self.angle)
    }

    /// The forward map: identity-space document pixels to where they land.
    ///
    /// Written out rather than composed from three glam affines because the
    /// order is the whole of it — scale *then* rotate, both about the pivot,
    /// then translate — and a composition read left to right is the one a
    /// reader gets backwards.
    pub fn matrix(&self) -> Affine {
        let m = self.rotation() * Mat2::from_diagonal(self.scale);
        let pivot = self.pivot();
        Affine {
            m,
            t: pivot + self.offset - m * pivot,
        }
    }

    /// The map the resampler wants: destination document pixels back to where
    /// they came from.
    ///
    /// Neither component of `scale` can be zero — see
    /// [`Transform::MIN_SCALE`] — so the matrix is always invertible. A flip
    /// makes the determinant negative and leaves it invertible, which is why
    /// nothing here has to know about one.
    pub fn inverse(&self) -> Affine {
        self.matrix().inverse()
    }

    /// The four corners of the transformed source, clockwise from top-left.
    ///
    /// The order matters to the caller that draws the box: consecutive corners
    /// have to share an edge, or the outline is a bow tie. It survives a flip
    /// without a special case, because an affine map takes adjacent corners to
    /// adjacent corners — a negative scale reverses the winding and leaves the
    /// cycle intact. `a_flipped_boxs_outline_is_not_a_bow_tie` pins it.
    pub fn quad(&self) -> [Vec2; 4] {
        let m = self.matrix();
        let (a, b) = (self.source.min, self.source.max);
        [
            m.apply(a),
            m.apply(Vec2::new(b.x, a.y)),
            m.apply(b),
            m.apply(Vec2::new(a.x, b.y)),
        ]
    }

    /// Where a handle sits, in document space.
    ///
    /// [`Handle::Move`] and [`Handle::Rotate`] answer with the centre: neither
    /// is drawn as a mark of its own, and the centre is what they both turn
    /// about.
    pub fn handle_at(&self, handle: Handle) -> Vec2 {
        let local = handle.local().unwrap_or(Vec2::ZERO);
        self.matrix().apply(self.pivot() + local * self.half())
    }

    /// Which handle a document-space point is grabbing.
    ///
    /// `tolerance` is a screen distance divided by the zoom, exactly as the
    /// polygon lasso's close distance is: a fixed document distance would be
    /// impossible to hit at 10% and impossible to avoid at 800%.
    ///
    /// The order is the rule. A corner beats an edge (see [`Handle::BOX`]), a
    /// handle beats the rotation around it, and the interior — tested by the
    /// *quad*, not by its bounding box — is a move. **Everything else is a
    /// rotation**, however far from the box it is.
    ///
    /// There is therefore no "hold of nothing" any more, which used to be how a
    /// click away from the box said "put this down". That reading now belongs
    /// to the pointer layer, where it can be told apart from the start of a
    /// rotation by whether the pointer travels — a distinction this function
    /// cannot make from one position, and one that has nothing to do with
    /// geometry. See `app.rs`.
    pub fn grab(&self, point: Vec2, tolerance: f32) -> Handle {
        for handle in Handle::BOX {
            if self.handle_at(handle).distance(point) <= tolerance {
                return handle;
            }
        }
        if self.contains(point) {
            Handle::Move
        } else {
            Handle::Rotate
        }
    }

    /// Is this document point inside the transformed box?
    ///
    /// Against the quad rather than its bounding rectangle: a box turned 45°
    /// has more corner outside it than inside, and a press there belongs to
    /// whatever is under the canvas, not to the transform.
    pub fn contains(&self, point: Vec2) -> bool {
        // Back into identity space, where the box is axis-aligned and the test
        // is two comparisons. Doing it the other way round would mean four
        // edge cross-products and a winding rule for a shape that is always
        // convex and always a rectangle.
        let local = self.inverse().apply(point);
        local.x >= self.source.min.x
            && local.x <= self.source.max.x
            && local.y >= self.source.min.y
            && local.y <= self.source.max.y
    }

    /// Carry out a drag of `handle` from `from` to `to`, both in document
    /// space.
    ///
    /// `uniform` constrains a corner drag to one scale on both axes, which is
    /// what Shift does everywhere else. It is ignored for an edge handle: an
    /// edge scales one axis by definition, and "uniform" would silently turn it
    /// into a corner.
    ///
    /// Absolute against `from` rather than accumulated per event: coming back
    /// to where the drag started comes back to exactly the transform it
    /// started with, and a drag that crosses the pivot cannot leave a residue
    /// of the frames it spent there.
    pub fn drag(&mut self, handle: Handle, from: Vec2, to: Vec2, uniform: bool) {
        match handle {
            Handle::Move => self.offset += to - from,
            Handle::Rotate => {
                // About the *current* centre, which is where the box is now
                // rather than where it started. Rotating about the identity
                // pivot would swing a moved box round the place it came from.
                let centre = self.pivot() + self.offset;
                let before = from - centre;
                let after = to - centre;
                if before.length_squared() > 1e-6 && after.length_squared() > 1e-6 {
                    // The angle the pointer swept about the centre since the
                    // *last* event, which is why a rotation is the one handle
                    // besides Move whose origin walks with the pointer — see
                    // the caller. Left absolute against the press, this `+=`
                    // adds the whole offset again on every event, so a hand
                    // held ten degrees off the grab point turns the box ten
                    // degrees per frame and it spins away. That was a real bug.
                    //
                    // Wrapped into a half turn either way so that crossing the
                    // ray behind the centre is a small step rather than a jump
                    // of 2π. Summing the steps is also what lets a drag wind
                    // past a full turn, which an absolute `atan2` cannot say.
                    let swept = after.y.atan2(after.x) - before.y.atan2(before.x);
                    self.angle += wrap_to_half_turn(swept);
                }
            }
            Handle::Scale { .. } => self.scale_to(handle, to, uniform),
        }
    }

    /// Move one handle to `to`, keeping the handle opposite it exactly where it
    /// is.
    ///
    /// The anchor staying put is the whole feel of a scale handle, and it is
    /// why this cannot be "multiply the scale by a ratio": the box grows away
    /// from the corner the other hand is not on, so the offset has to be
    /// recomputed to put the anchor back.
    fn scale_to(&mut self, handle: Handle, to: Vec2, uniform: bool) {
        let Some(local) = handle.local() else { return };
        let half = self.half();
        if half.x <= 0.0 || half.y <= 0.0 {
            return;
        }
        let anchor = self.handle_at(Handle::Scale {
            local: (-local.x as i8, -local.y as i8),
        });

        // Into the box's own frame, where the drag is two independent
        // distances. `to - anchor` spans the whole box, hence the 2.
        let in_frame = self.rotation().inverse() * (to - anchor);
        let wanted = in_frame / (2.0 * local * half);

        let mut scale = self.scale;
        if local.x != 0.0 {
            scale.x = Self::away_from_zero(wanted.x);
        }
        if local.y != 0.0 {
            scale.y = Self::away_from_zero(wanted.y);
        }
        if uniform && local.x != 0.0 && local.y != 0.0 {
            // The larger of the two, so the box follows the hand rather than
            // shrinking away from it: a drag that asks for 2× on one axis and
            // 1.5× on the other wanted a bigger box.
            //
            // *Magnitudes*, and each axis keeps its own sign. A plain `max`
            // was right only while both were positive: dragging a corner
            // through the anchor makes one negative, and `max` would then
            // always pick the axis that had not flipped and hand its sign to
            // both — so a Shift-drag past the corner either refused to flip or
            // flipped the axis that was not dragged that far.
            let s = scale.x.abs().max(scale.y.abs());
            scale = Vec2::new(s.copysign(scale.x), s.copysign(scale.y));
        }
        self.scale = scale;

        // Put the anchor back. `handle_at` reads `self.scale`, so this has to
        // run after the assignment above rather than being folded into it.
        self.offset = Vec2::ZERO;
        self.offset = anchor
            - self.handle_at(Handle::Scale {
                local: (-local.x as i8, -local.y as i8),
            });
    }

    /// `v`, pushed out to [`Self::MIN_SCALE`] if it is nearer zero than that,
    /// **keeping its sign**.
    ///
    /// Zero itself goes positive. It is the boundary rather than a direction,
    /// and a drag that lands exactly on the anchor has not said which side of
    /// it the hand is going.
    fn away_from_zero(v: f32) -> f32 {
        if v.abs() >= Self::MIN_SCALE {
            v
        } else if v < 0.0 {
            -Self::MIN_SCALE
        } else {
            Self::MIN_SCALE
        }
    }

    /// Mirror the floating pixels left to right, about the box's own centre.
    ///
    /// The box does not move: the source rectangle is symmetric about the
    /// pivot, so negating the x scale reflects the four corners onto each other
    /// and leaves the region they enclose exactly where it was. Only what is
    /// inside it turns round — which is the whole of what a flip is, and is why
    /// this needs no compensating offset the way a scale does.
    pub fn flip_x(&mut self) {
        self.scale.x = -self.scale.x;
    }

    /// Mirror the floating pixels top to bottom. See [`Self::flip_x`].
    pub fn flip_y(&mut self) {
        self.scale.y = -self.scale.y;
    }

    /// The pixels of the canvas the transformed source can reach.
    ///
    /// The bounding box of the *quad*, grown by [`SKIRT`] and clamped to the
    /// document. `None` when the transform has carried everything off the
    /// canvas, which is a real thing to do with a drag and means there is
    /// nothing to draw and nothing to commit.
    ///
    /// The quad rather than the source rectangle, for exactly the reason
    /// `StrokeBuilder::bounds` unions a dab's quad rather than its circle: a
    /// rectangle turned 45° reaches `half * sqrt(2)` from its centre, and a
    /// destination too tight leaves the corners of the mark uncommitted — where
    /// they redraw as a live preview and are then baked in by whatever edit
    /// comes next.
    pub fn dest_rect(&self, doc: UVec2) -> Option<PixelRect> {
        let mut bounds = Rect::empty();
        for corner in self.quad() {
            bounds.union_box(corner, Vec2::splat(SKIRT));
        }
        bounds.to_pixels_clamped(doc)
    }

    /// The rectangle a commit has to write, and therefore the one the undo
    /// patch has to span: everywhere the pixels went, **and** everywhere they
    /// came from.
    ///
    /// Both halves, always. A transform that only moved pixels away still
    /// changes the source — that is the hole it left — so a patch covering the
    /// destination alone would undo to a document with the hole still in it.
    pub fn damage(&self, doc: UVec2, lifted: bool) -> Option<PixelRect> {
        let dest = self.dest_rect(doc);
        if !lifted {
            return dest;
        }
        let source = self.source.to_pixels_clamped(doc);
        match (source, dest) {
            (Some(a), Some(b)) => Some(union(a, b)),
            (a, b) => a.or(b),
        }
    }
}

/// An angle brought into `-π..=π`.
///
/// What makes the difference of two `atan2` readings the *short* way round.
/// Without it, a pointer crossing the ray directly behind the centre reads as
/// very nearly a whole turn in the opposite direction, which is a box that
/// flips over as the hand passes six o'clock.
fn wrap_to_half_turn(radians: f32) -> f32 {
    let turn = std::f32::consts::TAU;
    let mut a = radians % turn;
    if a > std::f32::consts::PI {
        a -= turn;
    } else if a < -std::f32::consts::PI {
        a += turn;
    }
    a
}

/// The smallest rectangle containing both.
pub fn union(a: PixelRect, b: PixelRect) -> PixelRect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = (a.x + a.width).max(b.x + b.width);
    let bottom = (a.y + a.height).max(b.y + b.height);
    PixelRect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    }
}

/// A 2×3 affine map over document space: `p' = m * p + t`.
///
/// Its own type rather than `glam::Affine2` because it crosses into a uniform
/// buffer, and what goes in there has to be laid out by hand anyway — see the
/// uniform-layout note in CLAUDE.md. [`Affine::columns`] is that hand-off, and
/// it is the only place the packing is written down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    pub m: Mat2,
    pub t: Vec2,
}

impl Affine {
    pub const IDENTITY: Affine = Affine {
        m: Mat2::IDENTITY,
        t: Vec2::ZERO,
    };

    pub fn apply(&self, p: Vec2) -> Vec2 {
        self.m * p + self.t
    }

    /// The map that undoes this one.
    pub fn inverse(&self) -> Affine {
        let m = self.m.inverse();
        Affine {
            m,
            t: -(m * self.t),
        }
    }

    /// The two columns of `m` and the translation, which is what a shader
    /// uniform holds: three `vec2`s, because WGSL's `mat2x2` in a uniform block
    /// carries a 16-byte column stride and a Rust `[[f32; 2]; 2]` does not.
    pub fn columns(&self) -> [[f32; 2]; 3] {
        [
            self.m.x_axis.to_array(),
            self.m.y_axis.to_array(),
            self.t.to_array(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::vec2;

    const DOC: UVec2 = UVec2::splat(64);

    fn source() -> PixelRect {
        PixelRect {
            x: 10,
            y: 20,
            width: 20,
            height: 10,
        }
    }

    fn near(a: Vec2, b: Vec2, what: &str) {
        assert!(a.distance(b) < 1e-3, "{what}: {a:?} != {b:?}");
    }

    #[test]
    fn an_untouched_transform_moves_nothing() {
        let t = Transform::identity(source());
        assert!(t.is_identity());
        assert_eq!(t.matrix().m, Mat2::IDENTITY);
        near(t.matrix().t, Vec2::ZERO, "translation");
        near(t.pivot(), vec2(20.0, 25.0), "pivot");
        for corner in t.quad() {
            near(
                t.inverse().apply(t.matrix().apply(corner)),
                corner,
                "corner",
            );
        }
    }

    /// The shader walks the destination and asks the inverse where each pixel
    /// came from. If the two maps are not exact opposites the picture lands
    /// somewhere other than the box drawn round it — and the box is what the
    /// user is aiming with.
    #[test]
    fn a_transform_and_its_inverse_are_exact_opposites() {
        let mut t = Transform::identity(source());
        t.offset = vec2(7.5, -3.25);
        t.scale = vec2(2.0, 0.5);
        t.angle = 0.7;

        let f = t.matrix();
        let g = t.inverse();
        for p in [
            vec2(0.0, 0.0),
            vec2(10.0, 20.0),
            vec2(30.0, 30.0),
            vec2(-5.0, 63.0),
        ] {
            near(g.apply(f.apply(p)), p, "round trip");
            near(f.apply(g.apply(p)), p, "round trip the other way");
        }
    }

    /// A move is a move: every corner shifts by the same vector and nothing
    /// else changes.
    #[test]
    fn moving_carries_every_corner_by_the_same_amount() {
        let t = Transform::identity(source());
        let before = t.quad();
        let mut moved = t;
        moved.drag(Handle::Move, vec2(15.0, 25.0), vec2(21.0, 32.0), false);
        for (a, b) in before.iter().zip(moved.quad()) {
            near(b - *a, vec2(6.0, 7.0), "corner");
        }
        assert_eq!(moved.scale, Vec2::ONE);
        assert_eq!(moved.angle, 0.0);
    }

    /// The whole feel of a scale handle: the opposite corner does not move.
    /// Without it the box slides out from under the hand as it is resized.
    #[test]
    fn scaling_a_corner_pins_the_one_opposite_it() {
        let mut t = Transform::identity(source());
        let bottom_right = Handle::Scale { local: (1, 1) };
        let top_left = Handle::Scale { local: (-1, -1) };
        let anchor = t.handle_at(top_left);

        let grabbed = t.handle_at(bottom_right);
        t.drag(bottom_right, grabbed, vec2(50.0, 40.0), false);

        near(t.handle_at(top_left), anchor, "the anchor moved");
        near(t.handle_at(bottom_right), vec2(50.0, 40.0), "the handle");
    }

    /// An edge handle is one-dimensional. Dragging the middle of the top edge
    /// sideways must not widen the box.
    #[test]
    fn an_edge_handle_scales_one_axis_only() {
        let mut t = Transform::identity(source());
        let top = Handle::Scale { local: (0, -1) };
        t.drag(top, t.handle_at(top), vec2(40.0, 10.0), false);
        assert!((t.scale.x - 1.0).abs() < 1e-5, "x moved: {}", t.scale.x);
        assert!(t.scale.y > 1.5, "y did not follow: {}", t.scale.y);
    }

    /// Shift on a corner gives one scale for both axes, and the anchor still
    /// holds — the constraint must not be applied after the offset has been
    /// worked out from the unconstrained scale.
    #[test]
    fn a_uniform_corner_drag_keeps_both_axes_equal_and_the_anchor_still() {
        let mut t = Transform::identity(source());
        let corner = Handle::Scale { local: (1, 1) };
        let anchor = t.handle_at(Handle::Scale { local: (-1, -1) });
        t.drag(corner, t.handle_at(corner), vec2(60.0, 35.0), true);
        assert!((t.scale.x - t.scale.y).abs() < 1e-5, "{:?}", t.scale);
        near(
            t.handle_at(Handle::Scale { local: (-1, -1) }),
            anchor,
            "the anchor moved",
        );
    }

    /// A corner dragged past the anchor flips the picture, and the scale never
    /// reaches zero on the way through — the inverse map would be a division by
    /// zero and the destination rectangle a point.
    #[test]
    fn a_corner_dragged_through_the_anchor_flips_rather_than_stopping() {
        let mut t = Transform::identity(source());
        let corner = Handle::Scale { local: (1, 1) };
        t.drag(corner, t.handle_at(corner), vec2(-100.0, -100.0), false);
        assert!(t.scale.x < 0.0, "did not flip in x: {:?}", t.scale);
        assert!(t.scale.y < 0.0, "did not flip in y: {:?}", t.scale);
        assert!(t.scale.x.abs() >= Transform::MIN_SCALE, "{:?}", t.scale);
        assert!(t.scale.y.abs() >= Transform::MIN_SCALE, "{:?}", t.scale);
        // Still invertible, which is the point.
        near(
            t.inverse().apply(t.matrix().apply(Vec2::ZERO)),
            Vec2::ZERO,
            "invertible",
        );
        // And the handle still followed the hand, flip or no flip.
        near(t.handle_at(corner), vec2(-100.0, -100.0), "the handle");
    }

    /// Dragging the right edge past the left flips, and the *left* edge — the
    /// anchor — stays exactly where it was on the way through. A box that
    /// slides sideways as it passes through zero is one nobody can aim.
    #[test]
    fn dragging_an_edge_past_the_opposite_one_flips_about_it() {
        let right = Handle::Scale { local: (1, 0) };
        let left = Handle::Scale { local: (-1, 0) };
        let start = Transform::identity(source());
        let anchor = start.handle_at(left);
        let grabbed = start.handle_at(right);

        // Through zero and out the far side, one absolute drag at a time.
        for x in [25.0_f32, 10.0, 5.0, 0.0, -6.0, -20.0] {
            let mut t = start;
            t.drag(right, grabbed, vec2(x, grabbed.y), false);
            near(t.handle_at(left), anchor, "the anchor moved");
            assert!(
                t.scale.y > 0.0 && (t.scale.y - 1.0).abs() < 1e-5,
                "y followed x: {:?}",
                t.scale
            );
            assert!(t.scale.x.abs() >= Transform::MIN_SCALE, "{:?}", t.scale);
            let flipped = x < anchor.x;
            assert_eq!(
                t.scale.x < 0.0,
                flipped,
                "sign at x={x}: scale {:?}",
                t.scale
            );
        }
    }

    /// A flip mirrors the pixels and leaves the box exactly where it is: the
    /// four corners come back as the same four points, and the centre does not
    /// move. Flipping twice is the identity.
    #[test]
    fn a_flip_mirrors_the_pixels_and_leaves_the_box_where_it_is() {
        let mut t = Transform::identity(source());
        t.offset = vec2(6.0, -2.0);
        let before = t.quad();
        let centre = t.pivot() + t.offset;

        for flip in [Transform::flip_x as fn(&mut Transform), Transform::flip_y] {
            let mut f = t;
            flip(&mut f);
            assert!(!f.is_identity(), "a flip is a change");
            // The same four corners, in some order — the box is unmoved and
            // only what is inside it is turned round.
            for corner in f.quad() {
                assert!(
                    before.iter().any(|b| b.distance(corner) < 1e-3),
                    "corner {corner:?} is not one of {before:?}"
                );
            }
            let mid = f.quad().iter().fold(Vec2::ZERO, |a, b| a + *b) * 0.25;
            near(mid, centre, "the pivot moved");

            // And back again.
            flip(&mut f);
            assert_eq!(f.scale, t.scale, "flipping twice is not the identity");
            assert_eq!(f.offset, t.offset);
            for (a, b) in before.iter().zip(f.quad()) {
                near(b, *a, "corner after flipping back");
            }
        }
    }

    /// Consecutive corners have to share an edge whatever the sign of the
    /// scale, or the outline the interface draws is a bow tie.
    #[test]
    fn a_flipped_boxs_outline_is_not_a_bow_tie() {
        let mut t = Transform::identity(source());
        t.angle = 0.6;
        for scale in [vec2(-1.5, 1.0), vec2(1.0, -0.8), vec2(-2.0, -0.5)] {
            t.scale = scale;
            let q = t.quad();
            // Opposite sides of a parallelogram: 0→1 is the reverse of 2→3,
            // and 1→2 the reverse of 3→0. A bow tie fails both.
            near(q[1] - q[0], q[2] - q[3], "top and bottom edges");
            near(q[2] - q[1], q[3] - q[0], "left and right edges");
            // And the diagonals cross, which they cannot in a bow tie: the
            // midpoints of both are the centre.
            near((q[0] + q[2]) * 0.5, (q[1] + q[3]) * 0.5, "diagonals");
        }
    }

    /// A Shift-drag past the corner flips both axes together rather than
    /// letting the axis that did not flip hand its sign to the one that did.
    #[test]
    fn a_uniform_drag_through_the_corner_flips_both_axes() {
        let mut t = Transform::identity(source());
        let corner = Handle::Scale { local: (1, 1) };
        let anchor = t.handle_at(Handle::Scale { local: (-1, -1) });
        t.drag(corner, t.handle_at(corner), vec2(-40.0, -30.0), true);
        assert!(t.scale.x < 0.0 && t.scale.y < 0.0, "{:?}", t.scale);
        assert!((t.scale.x - t.scale.y).abs() < 1e-5, "{:?}", t.scale);
        near(
            t.handle_at(Handle::Scale { local: (-1, -1) }),
            anchor,
            "the anchor moved",
        );
    }

    /// A flipped transform still maps both ways exactly. The determinant
    /// changes sign and the matrix stays invertible, which is the whole reason
    /// nothing downstream of here needed a special case.
    #[test]
    fn a_flipped_transform_and_its_inverse_are_still_exact_opposites() {
        let mut t = Transform::identity(source());
        t.scale = vec2(-1.75, 0.6);
        t.angle = -0.4;
        t.offset = vec2(3.0, 9.0);
        let (f, g) = (t.matrix(), t.inverse());
        for p in [vec2(0.0, 0.0), vec2(30.0, 30.0), vec2(-5.0, 63.0)] {
            near(g.apply(f.apply(p)), p, "round trip");
            near(f.apply(g.apply(p)), p, "round trip the other way");
        }
        // The destination is the bounding box of the quad, and a flip's quad is
        // the same four points the unflipped one had — so the rectangle still
        // covers every corner, with no branch anywhere for the sign.
        let rect = t.dest_rect(DOC).expect("on the canvas");
        for corner in t.quad() {
            assert!(
                corner.x >= rect.x as f32
                    && corner.x <= (rect.x + rect.width) as f32
                    && corner.y >= rect.y as f32
                    && corner.y <= (rect.y + rect.height) as f32,
                "corner {corner:?} outside {rect:?}"
            );
        }
        assert!(t.damage(DOC, true).is_some());
    }

    /// Rotation turns about the centre the box has *now*, so a box that has
    /// been moved does not swing round where it started.
    #[test]
    fn rotating_a_moved_box_turns_about_where_it_is() {
        let mut t = Transform::identity(source());
        t.offset = vec2(20.0, 5.0);
        let centre = t.pivot() + t.offset;
        t.drag(
            Handle::Rotate,
            centre + vec2(10.0, 0.0),
            centre + vec2(0.0, 10.0),
            false,
        );

        assert!(
            (t.angle - std::f32::consts::FRAC_PI_2).abs() < 1e-4,
            "{}",
            t.angle
        );
        // The centre of the quad is the centre it turned about.
        let mid = t.quad().iter().fold(Vec2::ZERO, |a, b| a + *b) * 0.25;
        near(mid, centre, "the centre moved");
    }

    /// A rotation applies the angle the pointer swept and nothing more.
    ///
    /// The whole of the bug this guards: `drag` adds the offset between its two
    /// points, so a caller that leaves the origin at the press point adds the
    /// *same* offset on every event and the box accelerates away from the hand.
    /// Grabbing at 45° and moving to 50° is five degrees, however many events
    /// the pointer took to get there.
    #[test]
    fn a_rotation_applies_the_angle_the_pointer_swept() {
        let one_step = {
            let mut t = Transform::identity(source());
            let centre = t.pivot();
            let at = |deg: f32| centre + Vec2::from_angle(deg.to_radians()) * 30.0;
            t.drag(Handle::Rotate, at(45.0), at(50.0), false);
            t.angle
        };
        assert!(
            (one_step.to_degrees() - 5.0).abs() < 1e-3,
            "one step turned {} degrees",
            one_step.to_degrees()
        );

        // The same sweep in five events, with the origin walking as the caller
        // in `Editor::transform_moved` walks it, is the same five degrees.
        let mut t = Transform::identity(source());
        let centre = t.pivot();
        let at = |deg: f32| centre + Vec2::from_angle(deg.to_radians()) * 30.0;
        for i in 0..5 {
            let (a, b) = (45.0 + i as f32, 46.0 + i as f32);
            t.drag(Handle::Rotate, at(a), at(b), false);
        }
        assert!(
            (t.angle - one_step).abs() < 1e-3,
            "five events turned {} degrees, one turned {}",
            t.angle.to_degrees(),
            one_step.to_degrees()
        );
    }

    /// Winding the long way round is a sum of small steps, not a jump.
    ///
    /// Two things at once: crossing the ray behind the centre must not read as
    /// very nearly a whole turn backwards, and a drag that goes round more than
    /// once must be able to say so — which an absolute `atan2` never could.
    #[test]
    fn a_drag_can_wind_past_a_whole_turn() {
        let mut t = Transform::identity(source());
        let centre = t.pivot();
        let at = |deg: f32| centre + Vec2::from_angle(deg.to_radians()) * 30.0;
        for i in 0..90 {
            let (a, b) = (i as f32 * 5.0, (i + 1) as f32 * 5.0);
            t.drag(Handle::Rotate, at(a), at(b), false);
        }
        assert!(
            (t.angle.to_degrees() - 450.0).abs() < 1e-2,
            "a turn and a quarter came out as {} degrees",
            t.angle.to_degrees()
        );
    }

    #[test]
    fn an_angle_wraps_the_short_way_round() {
        use std::f32::consts::{PI, TAU};
        assert!((wrap_to_half_turn(0.1) - 0.1).abs() < 1e-6);
        assert!((wrap_to_half_turn(TAU - 0.1) + 0.1).abs() < 1e-6);
        assert!((wrap_to_half_turn(-TAU + 0.1) - 0.1).abs() < 1e-6);
        assert!(wrap_to_half_turn(PI - 0.01) > 0.0);
    }

    /// A quarter turn swaps the box's width and height. Anything that gets the
    /// pivot or the composition order wrong fails this.
    #[test]
    fn a_quarter_turn_swaps_the_extents() {
        let mut t = Transform::identity(source());
        t.angle = std::f32::consts::FRAC_PI_2;
        let rect = t.dest_rect(DOC).expect("on the canvas");
        // 20 x 10 becomes 10 x 20, plus a pixel of skirt on each side — and
        // then whatever `floor`/`ceil` make of a corner that a quarter turn in
        // f32 puts a millionth of a pixel past an integer, which is why this
        // allows one either way rather than naming 12 and 22 exactly.
        assert!(rect.width.abs_diff(12) <= 1, "width {}", rect.width);
        assert!(rect.height.abs_diff(22) <= 1, "height {}", rect.height);
    }

    /// The destination must cover the quad, not the source rectangle. A turned
    /// box reaches into the corners, and a rectangle too tight leaves the edge
    /// of the picture behind — the same failure the dab pass's bounds had.
    #[test]
    fn the_destination_covers_the_turned_quad() {
        let mut t = Transform::identity(PixelRect {
            x: 20,
            y: 20,
            width: 20,
            height: 20,
        });
        t.angle = std::f32::consts::FRAC_PI_4;
        let rect = t.dest_rect(DOC).expect("on the canvas");
        // Half-diagonal is 10 * sqrt(2) = 14.14, so the box spans about 28.3
        // about the centre at (30, 30): 15.8 .. 44.2, plus the skirt.
        assert!(rect.x <= 14, "left edge at {}", rect.x);
        assert!(
            rect.x + rect.width >= 46,
            "right edge at {}",
            rect.x + rect.width
        );
        for corner in t.quad() {
            assert!(
                corner.x >= rect.x as f32 && corner.x <= (rect.x + rect.width) as f32,
                "corner {corner:?} outside {rect:?}"
            );
        }
    }

    /// Everywhere the pixels went *and* everywhere they came from. A patch
    /// covering only the destination would undo to a document still holding
    /// the hole the lift left behind.
    #[test]
    fn the_damage_spans_the_source_and_the_destination() {
        let mut t = Transform::identity(source());
        t.offset = vec2(20.0, 20.0);
        let damage = t.damage(DOC, true).expect("something to commit");
        assert!(damage.x <= 10, "does not reach the source: {damage:?}");
        assert!(damage.y <= 20, "does not reach the source: {damage:?}");
        assert!(
            damage.x + damage.width >= 50 && damage.y + damage.height >= 50,
            "does not reach the destination: {damage:?}"
        );

        // A paste has no source to restore, so its damage is the destination
        // alone — writing the whole span would put an undo entry over pixels
        // the paste never touched.
        let pasted = t.damage(DOC, false).expect("something to commit");
        assert!(pasted.x >= 29, "reaches back to the source: {pasted:?}");
    }

    /// Dragged clean off the canvas there is nothing to draw and nothing to
    /// commit, and every caller has to be able to find that out without
    /// checking for a zero-area rectangle.
    #[test]
    fn a_transform_carried_off_the_canvas_has_no_destination() {
        let mut t = Transform::identity(source());
        t.offset = vec2(-500.0, -500.0);
        assert!(t.dest_rect(DOC).is_none());
        // But the damage is still real: the hole it left is on the canvas.
        assert!(t.damage(DOC, true).is_some());
    }

    /// The interior test is the quad's, not its bounding box's. A box turned
    /// 45° has more of its bounding box outside it than in, and a press there
    /// belongs to the canvas.
    #[test]
    fn a_press_in_the_corner_of_a_turned_box_is_not_inside_it() {
        let mut t = Transform::identity(PixelRect {
            x: 20,
            y: 20,
            width: 20,
            height: 20,
        });
        assert!(t.contains(vec2(30.0, 30.0)), "the middle is inside");
        assert!(
            t.contains(vec2(21.0, 21.0)),
            "and so is the corner, unturned"
        );
        t.angle = std::f32::consts::FRAC_PI_4;
        assert!(t.contains(vec2(30.0, 30.0)), "the middle is still inside");
        assert!(
            !t.contains(vec2(17.0, 17.0)),
            "the turned corner is outside"
        );
    }

    /// A corner beats the edge handle beside it, the interior is a move, and
    /// **everywhere else is a rotation** — beside a corner, beside an edge, and
    /// right across the canvas alike.
    #[test]
    fn a_corner_wins_over_the_edge_and_everywhere_outside_turns_the_box() {
        let t = Transform::identity(PixelRect {
            x: 20,
            y: 20,
            width: 20,
            height: 20,
        });
        let tol = 3.0;
        assert_eq!(
            t.grab(vec2(20.0, 20.0), tol),
            Handle::Scale { local: (-1, -1) }
        );
        assert_eq!(
            t.grab(vec2(30.0, 20.0), tol),
            Handle::Scale { local: (0, -1) }
        );
        assert_eq!(t.grab(vec2(30.0, 30.0), tol), Handle::Move);
        // Just outside a corner, just outside an edge, and far away: all the
        // same gesture. The last one used to be `None`, which is what "click
        // away from the box to put it down" was built on — see `Handle::Rotate`
        // for where that reading went.
        for outside in [
            vec2(15.0, 15.0),
            vec2(30.0, 14.0),
            vec2(0.0, 0.0),
            vec2(500.0, 500.0),
        ] {
            assert_eq!(t.grab(outside, tol), Handle::Rotate, "at {outside:?}");
        }
    }

    /// A rotation is offered outside the *quad*, not outside its bounding box.
    /// The corner of a turned box's bounding rectangle is outside the picture,
    /// so a press there turns it rather than moving it.
    #[test]
    fn the_turned_corner_of_a_bounding_box_turns_rather_than_moves() {
        let mut t = Transform::identity(PixelRect {
            x: 20,
            y: 20,
            width: 20,
            height: 20,
        });
        t.angle = std::f32::consts::FRAC_PI_4;
        assert_eq!(t.grab(vec2(30.0, 30.0), 3.0), Handle::Move);
        assert_eq!(t.grab(vec2(17.0, 17.0), 3.0), Handle::Rotate);
    }

    /// Coming back to where the drag began comes back to the transform it
    /// began with. A gesture accumulated per event drifts instead, and a drag
    /// that crosses the pivot leaves a residue of the frames it spent there.
    #[test]
    fn a_drag_is_absolute_against_where_it_started() {
        let start = Transform::identity(source());
        let corner = Handle::Scale { local: (1, 1) };
        let grabbed = start.handle_at(corner);

        let mut t = start;
        for to in [vec2(50.0, 40.0), vec2(12.0, 22.0), vec2(80.0, 5.0)] {
            t.drag(corner, grabbed, to, false);
        }
        t.drag(corner, grabbed, grabbed, false);
        near(t.handle_at(corner), grabbed, "handle");
        assert!((t.scale.x - 1.0).abs() < 1e-4, "{:?}", t.scale);
        assert!((t.scale.y - 1.0).abs() < 1e-4, "{:?}", t.scale);
        near(t.offset, Vec2::ZERO, "offset");
    }

    /// What the shader is handed. Three `vec2`s rather than a `mat2x2`, because
    /// a uniform-block matrix carries a 16-byte column stride on the WGSL side
    /// and a packed Rust array does not.
    #[test]
    fn the_columns_are_packed_as_the_shader_reads_them() {
        let a = Affine {
            m: Mat2::from_cols(vec2(1.0, 2.0), vec2(3.0, 4.0)),
            t: vec2(5.0, 6.0),
        };
        assert_eq!(a.columns(), [[1.0, 2.0], [3.0, 4.0], [5.0, 6.0]]);
        // And the map they describe is column-major, as glam and WGSL both are.
        near(a.apply(vec2(1.0, 0.0)), vec2(6.0, 8.0), "x axis");
        near(a.apply(vec2(0.0, 1.0)), vec2(8.0, 10.0), "y axis");
    }

    /// Re-seating leaves the map alone, exactly — which is what lets a text
    /// float grow as it is typed into without the words already on screen
    /// drifting under the hand that placed them.
    #[test]
    fn a_reseated_transform_maps_every_point_exactly_where_it_did() {
        let mut t = Transform::identity(source());
        t.offset = vec2(11.0, -4.5);
        t.scale = vec2(1.8, -0.6);
        t.angle = 0.9;
        let before = t.matrix();

        // Grown to the right and downwards, as a line of text grows.
        t.reseat(PixelRect {
            x: 10,
            y: 20,
            width: 57,
            height: 33,
        });
        let after = t.matrix();
        for p in [
            vec2(0.0, 0.0),
            vec2(10.0, 20.0),
            vec2(30.0, 30.0),
            vec2(-5.0, 63.0),
            vec2(200.0, -140.0),
        ] {
            near(after.apply(p), before.apply(p), "the map moved");
        }
        // And the *new* rectangle is what the box now describes.
        near(t.source().min, vec2(10.0, 20.0), "source min");
        near(t.source().max, vec2(67.0, 53.0), "source max");
    }

    /// At identity the correction is exactly zero, so a float that was typed
    /// into and never dragged still reads as unchanged and still records no
    /// edit.
    #[test]
    fn reseating_an_untouched_transform_leaves_it_untouched() {
        let mut t = Transform::identity(source());
        t.reseat(PixelRect {
            x: 4,
            y: 7,
            width: 90,
            height: 12,
        });
        assert!(t.is_identity(), "{t:?}");
        assert_eq!(t.offset, Vec2::ZERO);
    }

    #[test]
    fn a_union_covers_both_rectangles() {
        let a = PixelRect {
            x: 2,
            y: 3,
            width: 4,
            height: 5,
        };
        let b = PixelRect {
            x: 10,
            y: 1,
            width: 2,
            height: 2,
        };
        assert_eq!(
            union(a, b),
            PixelRect {
                x: 2,
                y: 1,
                width: 10,
                height: 7
            }
        );
    }
}
