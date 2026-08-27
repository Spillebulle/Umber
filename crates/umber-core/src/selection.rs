//! Selections: which pixels of the document an edit is allowed to touch.
//!
//! # What a selection *is*
//!
//! A [`Selection`] is **an outline plus a coverage mask**, and it is both
//! deliberately.
//!
//! The obvious representation is one byte per document pixel. It is simple and
//! it is what the GPU eventually needs, but it is also 4 MB on a 2048² canvas
//! and 100 MB on a 10000² one — a canvas Umber supports, and the reason
//! `band_rows` exists in the renderer. Almost all of that would be zero: a
//! selection is usually a small part of the picture. So the mask here is
//! **bounded to the selection's own pixel rectangle**, which makes a lasso
//! round one eye of a portrait cost what that eye covers rather than what the
//! portrait does.
//!
//! The other obvious representation is the path alone, tested per pixel with
//! point-in-polygon. That is exact and tiny, and it is the wrong thing to hand
//! a fragment shader: clipping happens once per fragment of every dab, and no
//! amount of cleverness makes "walk a thousand lasso segments" a per-fragment
//! cost. It also has no answer for a partly covered pixel, and a selection edge
//! without antialiasing is a staircase the artist can see.
//!
//! So: the mask is what gets used, and the outline is kept because the mask
//! cannot answer the two questions the outline can. Drawing the marching ants
//! from a mask would mean tracing a boundary back out of pixels — a second
//! algorithm, approximate where the path is exact. And a mask is tied to one
//! canvas size, where the rings are geometry and can be rasterised again.
//!
//! Combining two selections is the one thing that cannot hold to that, and
//! "What a boolean costs the outline" below says exactly what it gives up.
//! Feathering is the one thing that cannot be described by the rings *at all*,
//! and "Feather" below says how it is carried instead.
//!
//! # Antialiasing and the fill rule
//!
//! [`rasterise`] runs [`SUB_SCANLINES`] sub-scanlines per pixel row and
//! accumulates **exact** horizontal coverage of each span, so a vertical edge
//! is continuous and a horizontal one lands on one of five levels. That
//! asymmetry is deliberate: exact horizontal coverage is nearly free (it is
//! arithmetic on the span ends) where exact vertical coverage would mean
//! clipping polygons, and four sub-rows is enough that the difference is
//! invisible. An axis-aligned rectangle — much the most common selection —
//! comes out exact on *both* axes, because its horizontal edges fall between
//! sub-scanlines rather than through them.
//!
//! The fill rule is **nonzero winding**, not even-odd. A freehand lasso that
//! crosses itself is one region to the person who drew it; even-odd would
//! punch a hole in the middle of their own loop.
//!
//! # Combining selections
//!
//! A new shape replaces the selection, adds to it, takes it away, or keeps only
//! what the two have in common — [`SelectionOp`]. **The boolean happens on the
//! coverage, not on the rings.** Coverage is a rectangle of bytes: union is
//! `max`, difference is `min(a, 255 - b)`, intersection is `min(a, b)`, all
//! three exact per pixel and all three linear in the area touched. Polygon
//! boolean geometry is the alternative and it is a large, bug-prone algorithm —
//! self-intersection, coincident edges, degenerate vertices — for a result the
//! mask already has exactly.
//!
//! The three are the Kleene operators, which is what makes them the set that
//! composes: `max`, `min` and the complement, so intersect is difference's twin
//! rather than a fourth idea, and De Morgan holds between them pixel for pixel.
//! Rather than `a + b - ab` and its duals: they are idempotent, so adding a
//! shape to itself is the identity where the probabilistic pair would fatten it
//! a little every time; they are exact wherever either operand is fully in or
//! fully out, which is every pixel except the band at an edge; and they never
//! manufacture coverage the geometry does not have. What they cost is a seam:
//! two shapes that meet along a shared edge each cover it about half, and `max`
//! of two halves is a half rather than the whole the two together really do
//! cover. It is one pixel wide on an unfeathered edge, it is what Photoshop and
//! GIMP do, and the alternative is to keep the geometry — which is the thing
//! this avoids.
//!
//! **The bounding rectangle moves differently for each**, and getting it wrong
//! is silent — outside the rectangle is *not selected*, decided arithmetically
//! rather than by clamping, so a rectangle that is too small quietly deselects
//! and one that is too large costs a texture. Add takes the union of the two,
//! Intersect their overlap, Subtract keeps this one's; every result is then
//! trimmed to what is actually covered by [`Selection::from_mask`], so an
//! intersection that grazes a corner ends up as small as it really is.
//!
//! # What a boolean costs the outline
//!
//! The rings cannot survive it. Concatenating two ring lists is a union only
//! when the shapes are disjoint — where they overlap it draws a seam straight
//! through the middle of the merged region — and reversing a ring is a
//! difference only when it falls entirely inside. So after a boolean the rings
//! are **traced back out of the mask**, by [`trace_rings`], and that is the
//! second, approximate algorithm the section above says the rings exist to
//! avoid. Being honest about what that loses:
//!
//! - **The outline becomes pixel-quantised.** A lasso's smooth diagonal is a
//!   staircase once it has been added to something. Every application that
//!   draws marching ants from a mask has this, and the alternative is exactly
//!   the polygon boolean this design refused.
//! - **The geometry stops being resolution-independent.** Rasterising traced
//!   rings onto a different canvas reproduces the staircase rather than the
//!   curve. Nothing does that today — `Editor::apply_canvas` drops the
//!   selection outright on a resize, for the same reason it drops the history
//!   — so this is a door closed, not a behaviour lost.
//! - **The threshold is 50%.** [`Selection::contains`] answers off the traced
//!   ring and [`Selection::coverage_at`] off the mask, so along an antialiased
//!   edge the two can disagree by one pixel. They already could: `bounds` is
//!   the outline's box rounded outwards.
//!
//! A [`SelectionOp::Replace`] gesture — much the commonest — keeps its exact
//! rings. Tracing only ever runs where a boolean actually ran.
//!
//! # Feather
//!
//! A feather is a softening radius in document pixels: coverage falls from full
//! to nothing across a band `2 × radius` wide, centred on the outline.
//!
//! **It is a blur of the mask and the rings are left exactly where they were**,
//! which is the opposite of the decision a boolean forces and is right for the
//! same reason that one is. The kernel is symmetric, so the 50% contour of the
//! softened mask *is* the sharp edge it was blurred from — exactly so along a
//! straight edge, and within the curvature elsewhere. The rings are therefore
//! still the honest place to draw the marquee, and [`Selection::contains`]
//! still answers where the fill is half. Tracing a feathered mask would put the
//! outline in the same place, pixel-quantised, having thrown away the exact
//! geometry to get there.
//!
//! **The radius is a field**, because rings alone cannot describe a soft edge
//! and the rings are re-rasterised twice in Umber: `Selection::flipped` when
//! the canvas is mirrored, and `Editor::carry_selection` when a transform
//! commits. Both rebuild the mask, so both have to re-apply the feather or a
//! flip would quietly harden every soft edge in the picture. What a boolean
//! records is the **larger** of the two radii — the softest edge in the
//! result — because one number cannot say that half an outline is soft, and of
//! the two answers available only that one cannot harden an edge that was soft.
//! The drift it admits is small and real: subtracting a feathered shape from a
//! hard selection and then flipping the canvas softens the whole outline.
//!
//! The kernel is a **tent**, two box passes of half-width `radius / 2` **per
//! axis** — four passes, not two, because a tent is the box convolved with
//! itself and convolution is per axis; one pass along each is a two-dimensional
//! box, which reaches half as far as the radius asked for and has a corner in
//! its profile. Three properties earn it: the running sums make it linear in the
//! area whatever the radius; every partial sum is an exact integer, so the only
//! rounding is the one store per pass and the result is exactly symmetric; and
//! being separable over a mask that is itself separable, an axis-aligned
//! rectangle keeps the exactness the fill rule gives it — its softened coverage
//! is the product of two identical one-dimensional ramps, the same on both
//! axes, mirrored exactly about the rectangle's centre. A Gaussian buys a
//! smoother shoulder for a kernel with no compact support, which would have to
//! be truncated somewhere and would then be neither exact nor symmetric.
//!
//! **Outside the canvas counts as unselected**, so a selection reaching the
//! edge of the document fades at it. That is what Photoshop and GIMP do, and
//! the alternative — treating the canvas edge as more of the same — would make
//! a feathered Select All paint a hard edge the artist never drew.
//!
//! **A radius of zero is the exact identity**: the same bounds, the same bytes,
//! no allocation and no pass, so a selection nobody feathered costs exactly
//! what it did before feathering existed.
//!
//! # What a feather does to a lift, which is not new but is now the ordinary
//! case
//!
//! `transform.wgsl`'s `fs_mask` takes the share of a pixel that leaves as
//! `min(a, m) / a` rather than `a × m`, because painting is *already* clipped
//! by this same mask in the dab pass and multiplying applies it twice. The
//! third of the three cases that rule enumerates — `a < m`, content softer than
//! the mask — reads "the float takes it whole, with that falloff intact rather
//! than multiplied by the mask's". Against an antialiased edge that is a
//! one-pixel band. Against a feather it is a band `2 × radius` wide, so
//! **lifting a soft wash through a wide feather takes it whole wherever it is
//! fainter than the ramp**, and the feather shows only where the content is
//! more opaque than the mask.
//!
//! That is the documented rule behaving as documented and nothing here changed
//! it, but it is worth saying out loud in the place a feather is defined,
//! because a feather is exactly what somebody reaches for when they want a soft
//! edge on a lift. `a_lift_still_splits_paint_the_selection_did_not_make`
//! refuses the tempting over-correction — taking everything the mask touches —
//! and the argument for `min` is in `transform.wgsl`'s own comments; changing
//! it is a decision about the transform, not about this module.
//!
//! # What is not here
//!
//! No "select by colour", no growing or shrinking a selection by a distance,
//! and no saving one into the document. Each is a real feature and none is
//! drawn in the interface, which is the rule this file lives by as much as any
//! other.

use crate::geom::{FlipAxis, PixelRect, Rect};
use glam::{UVec2, Vec2};

/// Sub-scanlines per pixel row. See the module docs.
const SUB_SCANLINES: u32 = 4;

/// Coverage at or above which a pixel counts as inside, for [`trace_rings`].
///
/// Half. A traced outline has to fall somewhere in the antialiased band and the
/// middle of it is the only choice that treats the two sides alike.
const INSIDE: u8 = 128;

/// How a selection outline is drawn.
///
/// One tool with a mode rather than four tools: they produce the same thing
/// and differ only in the gesture, so four entries in the rail would be four
/// names for one selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SelectionMode {
    /// Drag a box. Its corners can be rounded — see
    /// [`Selection::rounded_rectangle`].
    #[default]
    Rectangle,
    /// Drag a box and take the ellipse inscribed in it.
    ///
    /// Deliberately its own mode rather than [`SelectionMode::Rectangle`] at
    /// full roundness. A fully rounded *rectangle* is a stadium — two straight
    /// flanks joined by half-discs — and only a square one is a circle, where
    /// an ellipse is the curve through all four midpoints and is a different
    /// shape everywhere the box is not square. Folding the two together would
    /// mean the picker had no way to ask for the shape every other application
    /// calls an elliptical marquee.
    Ellipse,
    /// Freehand — the outline follows the pointer.
    Lasso,
    /// Click point to point; each click adds a straight edge. Usually called a
    /// polygonal lasso.
    Polygon,
}

impl SelectionMode {
    /// Every mode, in the order the picker lists them.
    ///
    /// Hand-written, and `every_mode_is_in_all_where_the_match_puts_it` indexes
    /// it from an exhaustive match rather than walking it — a test that walked
    /// `ALL` could only ever check what is already in it.
    ///
    /// **The hole is named rather than denied**, which is `docs`' rule for this
    /// exact shape and which an earlier draft of this comment got wrong. A
    /// fifth variant left out of `ALL` does *not* fail that test: the loop is
    /// over `ALL`, so it never evaluates the new arm and never indexes out of
    /// bounds. What forces a new variant to be dealt with is
    /// [`SelectionMode::label`], [`SelectionMode::hint`] and
    /// [`SelectionMode::extra`], each exhaustive with no catch-all. What the
    /// array's guard catches is a variant filed in the **wrong position**,
    /// which is the likelier slip and the one nothing else can see.
    pub const ALL: [SelectionMode; 4] =
        [Self::Rectangle, Self::Ellipse, Self::Lasso, Self::Polygon];

    pub fn label(self) -> &'static str {
        match self {
            Self::Rectangle => "Rectangle",
            Self::Ellipse => "Ellipse",
            Self::Lasso => "Lasso",
            Self::Polygon => "Polygon",
        }
    }

    /// What the gesture is, for the options strip.
    pub fn hint(self) -> &'static str {
        match self {
            Self::Rectangle => "Drag a box.",
            Self::Ellipse => "Drag a box; the ellipse inside it is selected.",
            Self::Lasso => "Draw round it freehand.",
            Self::Polygon => {
                "Click point to point. Click the first point again, or press \
                 Enter, to close the shape."
            }
        }
    }

    /// The one setting this mode has of its own, if it has one.
    ///
    /// Two of the four do — the rectangle's corner roundness and the lasso's
    /// stabiliser — and a strip that drew both whatever the mode would be two
    /// controls doing nothing for half of what the picker offers. Which modes
    /// have one is a property of the *gesture* rather than of the drawing (a
    /// box has corners to round, a freehand line has tremor to damp), which is
    /// why it is stated here and not in `ui.rs`.
    ///
    /// Exhaustive rather than a `matches!`, for the reason `EditKind::label`
    /// is: a fifth mode must state whether it has one instead of silently
    /// answering `None`.
    pub fn extra(self) -> Option<ModeSetting> {
        match self {
            Self::Rectangle => Some(ModeSetting::Roundness),
            Self::Ellipse => None,
            Self::Lasso => Some(ModeSetting::Stabiliser),
            Self::Polygon => None,
        }
    }
}

/// The one setting a [`SelectionMode`] has of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModeSetting {
    /// How far the rectangle's corners are rounded: `0.0` square through `1.0`
    /// stadium. See [`Selection::rounded_rectangle`].
    Roundness,
    /// How heavily the lasso damps the hand: `0.0` off through
    /// [`SelectionDraft::MAX_STABILISER`].
    Stabiliser,
}

/// What a new shape does to the selection already standing.
///
/// Which modifier means which is the interface's, not the engine's — see
/// `app.rs` — because it has to be reconciled with what Alt already does on the
/// canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SelectionOp {
    /// The new shape *is* the selection. What an unmodified gesture does.
    #[default]
    Replace,
    /// Union. Two disjoint areas can both be selected; two that touch become
    /// one region with one outline.
    Add,
    /// Difference. The new shape is taken out of what was selected.
    Subtract,
    /// Intersection. Only what both cover stays selected.
    Intersect,
}

impl SelectionOp {
    pub const ALL: [SelectionOp; 4] = [Self::Replace, Self::Add, Self::Subtract, Self::Intersect];

    pub fn label(self) -> &'static str {
        match self {
            Self::Replace => "Replace",
            Self::Add => "Add",
            Self::Subtract => "Subtract",
            Self::Intersect => "Intersect",
        }
    }

    /// What the operation does, for the control that offers it.
    ///
    /// Each says what happens to the selection *already standing*, because that
    /// is the half a name like "Intersect" does not carry on its own.
    pub fn hint(self) -> &'static str {
        match self {
            Self::Replace => "The new shape becomes the selection.",
            Self::Add => "The new shape joins what is already selected.",
            Self::Subtract => "The new shape is taken out of what is selected.",
            Self::Intersect => "Only what both the new shape and the selection cover stays.",
        }
    }
}

/// A region of the document, as an outline and a coverage mask over the
/// outline's bounding rectangle.
#[derive(Clone, Debug, PartialEq)]
pub struct Selection {
    /// Closed rings in document space. The closing edge is implicit: the last
    /// point joins the first, so a ring is never stored with its start
    /// repeated.
    rings: Vec<Vec<Vec2>>,
    bounds: PixelRect,
    /// `bounds.width * bounds.height` bytes, row-major from `bounds`'s
    /// top-left. `0` is outside, `255` is fully inside.
    coverage: Vec<u8>,
    /// The softening radius already applied to `coverage`, in document pixels.
    ///
    /// Kept because the rings cannot describe it and the rings are rasterised
    /// again on a canvas flip and on a transform commit — see "Feather" in the
    /// module docs. Zero is the ordinary case and the exact identity.
    feather: f32,
}

impl Selection {
    /// The widest feather a selection may carry, in document pixels.
    ///
    /// A bound rather than a taste: the radius decides how far the bounding
    /// rectangle grows and therefore how much texture a selection costs, and
    /// the number reaches this module from a control somebody can type into.
    /// 250 px is already a falloff five hundred pixels wide, which is larger
    /// than any edge a painter is softening.
    pub const MAX_FEATHER: f32 = 250.0;

    /// Build a selection from closed rings in document space.
    ///
    /// Returns `None` when nothing of the shape lands on the canvas — an empty
    /// selection and no selection are the same thing to every caller, and
    /// making that an `Option` here means none of them has to check for a
    /// zero-area rectangle later.
    pub fn from_rings(rings: Vec<Vec<Vec2>>, doc: UVec2) -> Option<Self> {
        let mut extent = Rect::empty();
        for ring in &rings {
            for p in ring {
                extent.union_box(*p, Vec2::ZERO);
            }
        }
        let bounds = extent.to_pixels_clamped(doc)?;
        let coverage = rasterise(&rings, bounds);
        // A shape thinner than a pixel can have a bounding rect and no
        // coverage at all. That is nothing selected, which is `None`.
        if coverage.iter().all(|c| *c == 0) {
            return None;
        }
        Some(Self {
            rings,
            bounds,
            coverage,
            feather: 0.0,
        })
    }

    /// An axis-aligned box between two corners, in either order.
    pub fn rectangle(a: Vec2, b: Vec2, doc: UVec2) -> Option<Self> {
        let min = a.min(b);
        let max = a.max(b);
        Self::from_rings(
            vec![vec![
                min,
                Vec2::new(max.x, min.y),
                max,
                Vec2::new(min.x, max.y),
            ]],
            doc,
        )
    }

    /// An axis-aligned box with its corners rounded by `roundness`, `0.0`
    /// square through `1.0`.
    ///
    /// The radius is `roundness × half the shorter side`, so `1.0` is the
    /// largest round corner the box can hold: a circle where the box is
    /// square, a stadium where it is not. Anything larger would make the two
    /// arcs on the short side overlap, which is a shape with no boundary
    /// rather than a rounder one.
    ///
    /// **A roundness at or below zero is the exact identity**, hard-wired to
    /// [`Selection::rectangle`] rather than arrived at by an arc of zero
    /// radius. Two reasons, and the second is the one that matters: a
    /// zero-radius arc still emits its endpoints, so the ring would carry eight
    /// coincident-in-pairs vertices where the ordinary rectangle carries four,
    /// and the rectangle is the one shape [`rasterise`] is exact on *both* axes
    /// for — a promise a degenerate ring is not obliged to keep. The commonest
    /// selection there is therefore costs exactly what it did before this
    /// existed.
    ///
    /// The ring runs clockwise in document space (y down), like
    /// [`Selection::rectangle`]'s and [`Selection::ellipse`]'s, at every
    /// roundness. **Nothing observable depends on that today** and the
    /// handedness is kept anyway: the fill rule is nonzero winding, and
    /// [`Selection::contains`] tests the sum against zero, so a single ring
    /// answers identically whichever way round it runs. What it buys is that
    /// the three constructors are comparable, and that anything which ever
    /// concatenates rings — nothing does, because a boolean traces new ones out
    /// of the mask — would see one handedness rather than two. Said as a
    /// property rather than as a guarantee, because an earlier draft of this
    /// comment claimed a reversed ring would change what `contains` answered,
    /// and it would not.
    pub fn rounded_rectangle(a: Vec2, b: Vec2, roundness: f32, doc: UVec2) -> Option<Self> {
        let mut ring = Vec::new();
        rounded_rect_ring(a, b, roundness, &mut ring);
        Self::from_rings(vec![ring], doc)
    }

    /// The ellipse inscribed in the box between two corners, in either order.
    ///
    /// Not a rounded rectangle at full roundness: see [`SelectionMode::Ellipse`]
    /// for why the two are different shapes.
    pub fn ellipse(a: Vec2, b: Vec2, doc: UVec2) -> Option<Self> {
        let mut ring = Vec::new();
        ellipse_ring(a, b, &mut ring);
        Self::from_rings(vec![ring], doc)
    }

    /// One closed ring through `points`, which is what both the lasso and the
    /// polygonal lasso produce — they differ in how the points were gathered,
    /// not in what they mean.
    pub fn polygon(points: &[Vec2], doc: UVec2) -> Option<Self> {
        if points.len() < 3 {
            return None;
        }
        Self::from_rings(vec![points.to_vec()], doc)
    }

    /// The pixel rectangle the selection covers. Never zero-area.
    pub fn bounds(&self) -> PixelRect {
        self.bounds
    }

    /// The mask, row-major over [`Selection::bounds`].
    pub fn coverage(&self) -> &[u8] {
        &self.coverage
    }

    /// Coverage at one *document* pixel. Outside the bounds is outside the
    /// selection, which is `0` rather than a panic — callers walk rectangles
    /// that need not line up with this one.
    pub fn coverage_at(&self, x: u32, y: u32) -> u8 {
        let b = self.bounds;
        if x < b.x || y < b.y || x >= b.x + b.width || y >= b.y + b.height {
            return 0;
        }
        let i = (y - b.y) as usize * b.width as usize + (x - b.x) as usize;
        self.coverage[i]
    }

    /// Is this document point inside the outline?
    ///
    /// Answered from the **path**, by nonzero winding, not from the mask: this
    /// is what a hit test wants — "did the user press inside the selection" —
    /// and reading a rounded byte would put the boundary half a pixel away from
    /// where the outline is drawn.
    pub fn contains(&self, point: Vec2) -> bool {
        self.rings.iter().map(|r| winding(r, point)).sum::<i32>() != 0
    }

    /// The closed rings, for drawing the outline.
    pub fn rings(&self) -> &[Vec<Vec2>] {
        &self.rings
    }

    /// The softening radius this selection's mask already carries, in document
    /// pixels. Zero for every selection nobody feathered.
    pub fn feather(&self) -> f32 {
        self.feather
    }

    /// This selection with its edge softened by `radius` document pixels.
    ///
    /// **The rings are kept exactly**, and the module docs have the argument:
    /// the kernel is symmetric, so the 50% contour of the result is the sharp
    /// edge it was blurred from, and that is where the marquee belongs.
    ///
    /// **Not idempotent, deliberately.** It blurs whatever coverage is there
    /// and *records* the radius it was given, so calling it twice softens
    /// twice. Every caller in Umber hands it a sharp mask: a gesture's own
    /// shape, or the re-rasterisation a flip and a transform commit do. Asking
    /// it to notice `self.feather` and blur by the difference would be a
    /// promise the tent cannot keep — two tents are not one wider tent.
    ///
    /// A radius at or below zero is the exact identity: the same bounds, the
    /// same bytes, **no allocation and no copy** — which is why this takes
    /// `self` by value rather than borrowing it. Every caller owns the
    /// selection it is softening (a gesture's own shape, or the rebuild a flip
    /// or a transform commit just made), so the common case is a move, and
    /// consuming the sharp mask is also what stops the non-idempotence above
    /// being reached by accident.
    ///
    /// `None` where a shape is so thin that softening it rounds every pixel to
    /// nothing — the same answer as no selection everywhere else in this file.
    ///
    /// **A mask that survives but never reaches [`INSIDE`] is kept**, where
    /// [`Selection::from_mask`] refuses one. The two look like the same
    /// condition and are answers to different questions: there the *outline* is
    /// what could not be found, and a selection with no outline is one nobody
    /// can see; here the outline is the exact geometry of the shape somebody
    /// drew and is unchanged, and a very soft selection whose peak coverage is
    /// low is precisely what a feather wider than the shape means. Refusing it
    /// would make the rail stop working part way along for no reason the artist
    /// could see.
    #[must_use]
    pub fn feathered(self, radius: f32, doc: UVec2) -> Option<Self> {
        let radius = radius.clamp(0.0, Self::MAX_FEATHER);
        if radius <= 0.0 {
            return Some(self);
        }
        let (bounds, coverage) = soften(self.bounds, &self.coverage, radius, doc)?;
        Some(Self {
            rings: self.rings,
            bounds,
            coverage,
            feather: radius,
        })
    }

    /// The same selection on a canvas that has been mirrored.
    ///
    /// The rings are geometry, so this is the mirror applied to them and a
    /// re-rasterisation — exactly what `Editor::carry_selection` does when a
    /// transform commits, and for exactly the same reason: an outline left
    /// where the picture used to be is one that lies about what it covers.
    /// Nothing here rasterises: [`Selection::from_rings`] is the one
    /// rasteriser, and a second one that mirrored the *mask* would have to
    /// agree with it about the antialiased band along every edge.
    ///
    /// Mirroring reverses the winding of every ring, which the nonzero rule
    /// does not care about — a winding number of -1 is as inside as +1.
    ///
    /// **The feather is re-applied**, because the rebuilt mask is the sharp
    /// rasterisation of the mirrored rings and a flip that hardened every soft
    /// edge in the picture would be a silent loss. The radius is a scalar and
    /// a mirror is an isometry, so it needs no transforming — see "Feather" in
    /// the module docs.
    ///
    /// **A feather that dissolves the mirror keeps the hard one instead**, and
    /// this is not a hypothetical. A boolean traces its rings at the 50%
    /// contour and records the *larger* of the two radii, so a small region
    /// carrying a wide feather is reachable — intersect two heavily feathered
    /// shapes that barely overlap — and re-rasterising those rings sharp and
    /// softening them by that radius can round every pixel to nothing.
    /// Deleting somebody's selection because they flipped the canvas is far
    /// worse than mirroring it hard, and it would not even be undoable: undoing
    /// a flip *is* another flip, so there is nothing to bring it back.
    ///
    /// `None` therefore still means only what it means everywhere else in this
    /// file: the mirrored rings enclosed no pixel at all, which a mirror of
    /// something with area cannot produce.
    pub fn flipped(&self, axis: FlipAxis, doc: UVec2) -> Option<Self> {
        let size = Vec2::new(doc.x as f32, doc.y as f32);
        let rings = self
            .rings
            .iter()
            .map(|ring| ring.iter().map(|p| axis.mirror(*p, size)).collect())
            .collect();
        let sharp = Self::from_rings(rings, doc)?;
        Some(sharp.clone().feathered(self.feather, doc).unwrap_or(sharp))
    }

    /// Build a selection from a coverage mask, trimming it to what is actually
    /// covered and tracing its outline.
    ///
    /// The three booleans below all end here, which is what keeps the trim and
    /// the trace in one place. Trimming is not tidiness: a difference leaves
    /// empty rows and columns behind and an intersection is usually far smaller
    /// than either operand, and `bounds` is what sizes the texture the dab pass
    /// samples, so an untrimmed mask would upload the hole it just cut. It is
    /// also how "subtracted down to nothing" becomes `None`, which is the same
    /// answer as no selection everywhere else in this file.
    ///
    /// `feather` is carried rather than derived: the mask handed in is already
    /// as soft as its operands were, and what this records is the radius a
    /// later re-rasterisation has to put back.
    fn from_mask(bounds: PixelRect, coverage: Vec<u8>, feather: f32) -> Option<Self> {
        let (bounds, coverage) = trim(bounds, coverage)?;
        let rings = trace_rings(bounds, &coverage);
        // A mask can be non-empty and still trace nothing: coverage below
        // `INSIDE` everywhere is a selection so faint no outline can be drawn
        // for it, and an outline nobody can see is a selection nobody can find.
        if rings.is_empty() {
            return None;
        }
        Some(Self {
            rings,
            bounds,
            coverage,
            feather,
        })
    }

    /// The feather a combination of these two carries: the softer of the pair.
    ///
    /// One number cannot say that half an outline is soft, and of the two
    /// answers available this is the one that cannot harden an edge that was
    /// soft. See "Feather" in the module docs.
    fn shared_feather(&self, other: &Self) -> f32 {
        self.feather.max(other.feather)
    }

    /// This selection with `other` added to it.
    ///
    /// The bounding rectangle **grows**, so the mask is reallocated and
    /// re-origined against the union of the two — which is the one place an
    /// off-by-one here would put every selected pixel one across.
    pub fn union(&self, other: &Self) -> Option<Self> {
        let bounds = union_rects(self.bounds, other.bounds);
        let mut coverage = vec![0u8; bounds.area() as usize];
        // `max`, not a probabilistic add: see the module docs.
        self.blit_into(&mut coverage, bounds, |dst, src| dst.max(src));
        other.blit_into(&mut coverage, bounds, |dst, src| dst.max(src));
        Self::from_mask(bounds, coverage, self.shared_feather(other))
    }

    /// This selection with `other` taken out of it.
    ///
    /// The bounds can only shrink, so the mask starts as this one's and
    /// [`Selection::from_mask`] trims whatever the cut emptied.
    pub fn difference(&self, other: &Self) -> Option<Self> {
        let mut coverage = self.coverage.clone();
        // `min(a, 255 - b)`, the dual of the `max` above: where `other` is
        // fully in, nothing survives; where it is half in, at most half does.
        other.blit_into(&mut coverage, self.bounds, |dst, src| dst.min(255 - src));
        Self::from_mask(self.bounds, coverage, self.shared_feather(other))
    }

    /// Only what both this selection and `other` cover.
    ///
    /// The bounds can only **shrink**, to the overlap of the two rectangles —
    /// and where they do not overlap at all there is nothing to build, which
    /// is `None` rather than a zero-area rectangle the renderer must not be
    /// handed. Starting from the overlap rather than from this selection's own
    /// rectangle is not an optimisation: `blit_into` only ever visits the
    /// overlap, so a mask sized to the larger rectangle would keep this
    /// selection's coverage untouched everywhere `other` does not reach — which
    /// is a union of the two things intersect is supposed to reject.
    pub fn intersection(&self, other: &Self) -> Option<Self> {
        let bounds = overlap_rect(self.bounds, other.bounds)?;
        // Seeded with this selection's own coverage, then bounded by the
        // other's: `min(a, b)`, the twin of the difference's `min(a, 255 - b)`.
        let mut coverage = vec![0u8; bounds.area() as usize];
        self.blit_into(&mut coverage, bounds, |_, src| src);
        other.blit_into(&mut coverage, bounds, |dst, src| dst.min(src));
        Self::from_mask(bounds, coverage, self.shared_feather(other))
    }

    /// Apply `op` to the selection standing, with the shape just drawn.
    ///
    /// Every empty case is decided here rather than at the call site, because
    /// they do not all answer the same way. A `Replace` that encloses nothing
    /// **deselects** — a bare click is how every paint application spells that.
    /// An `Add` or a `Subtract` that encloses nothing leaves the selection
    /// exactly as it was: a slip of the hand while holding a modifier is not a
    /// request to throw the work away. And subtracting from nothing is nothing,
    /// because "no selection" means the whole document and taking a shape out
    /// of it would be a bigger claim than the gesture made.
    ///
    /// **Intersecting with nothing is the shape**, and it is the one empty case
    /// that does not follow Subtract's. No selection means the whole document,
    /// and the whole document intersected with a shape is exactly that shape —
    /// no more than the gesture drew, which is the test Subtract fails. So the
    /// first intersect of a session behaves as a replace, which is also what it
    /// means.
    ///
    /// `shape` is taken by value so `Replace` — much the commonest — moves it
    /// through untouched, with no copy of the mask and no outline traced.
    pub fn combined(base: Option<&Self>, shape: Option<Self>, op: SelectionOp) -> Option<Self> {
        match (op, base, shape) {
            (SelectionOp::Replace, _, shape) => shape,
            (SelectionOp::Subtract, None, _) => None,
            (SelectionOp::Add | SelectionOp::Intersect, None, shape) => shape,
            (_, Some(base), None) => Some(base.clone()),
            (SelectionOp::Add, Some(base), Some(shape)) => base.union(&shape),
            (SelectionOp::Subtract, Some(base), Some(shape)) => base.difference(&shape),
            (SelectionOp::Intersect, Some(base), Some(shape)) => base.intersection(&shape),
        }
    }

    /// Combine this selection's coverage into `dst`, which spans `rect`.
    ///
    /// Row by row over the overlap only, so a small shape added to a large
    /// selection costs the small shape rather than the large one.
    fn blit_into(&self, dst: &mut [u8], rect: PixelRect, mut f: impl FnMut(u8, u8) -> u8) {
        let x0 = self.bounds.x.max(rect.x);
        let y0 = self.bounds.y.max(rect.y);
        let x1 = (self.bounds.x + self.bounds.width).min(rect.x + rect.width);
        let y1 = (self.bounds.y + self.bounds.height).min(rect.y + rect.height);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let span = (x1 - x0) as usize;
        for y in y0..y1 {
            let d = (y - rect.y) as usize * rect.width as usize + (x0 - rect.x) as usize;
            let s = (y - self.bounds.y) as usize * self.bounds.width as usize
                + (x0 - self.bounds.x) as usize;
            for i in 0..span {
                dst[d + i] = f(dst[d + i], self.coverage[s + i]);
            }
        }
    }
}

/// The smallest rectangle holding both.
fn union_rects(a: PixelRect, b: PixelRect) -> PixelRect {
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

/// The rectangle both cover, or `None` where they do not meet.
///
/// A zero-width or zero-height overlap is `None` rather than an empty
/// rectangle: two selections that share only an edge have no pixel in common,
/// and every other function here treats "nothing selected" as `None`.
fn overlap_rect(a: PixelRect, b: PixelRect) -> Option<PixelRect> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = (a.x + a.width).min(b.x + b.width);
    let bottom = (a.y + a.height).min(b.y + b.height);
    if right <= x || bottom <= y {
        return None;
    }
    Some(PixelRect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    })
}

/// Shrink `rect` to the rows and columns of `coverage` that hold anything, and
/// re-origin the mask onto it. `None` when nothing is covered at all.
fn trim(rect: PixelRect, coverage: Vec<u8>) -> Option<(PixelRect, Vec<u8>)> {
    let w = rect.width as usize;
    let mut min_x = w;
    let mut max_x = 0usize;
    let mut min_y = rect.height as usize;
    let mut max_y = 0usize;
    for (y, row) in coverage.chunks_exact(w).enumerate() {
        let first = row.iter().position(|c| *c != 0);
        let Some(first) = first else { continue };
        // `rposition` from the end rather than a second scan of the whole row:
        // an interior row of a large selection is covered edge to edge, and
        // both ends are then found in one step each.
        let last = row.iter().rposition(|c| *c != 0).unwrap_or(first);
        min_x = min_x.min(first);
        max_x = max_x.max(last);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    if min_x > max_x || min_y > max_y {
        return None;
    }
    let trimmed = PixelRect {
        x: rect.x + min_x as u32,
        y: rect.y + min_y as u32,
        width: (max_x - min_x + 1) as u32,
        height: (max_y - min_y + 1) as u32,
    };
    if trimmed == rect {
        return Some((rect, coverage));
    }
    let tw = trimmed.width as usize;
    let mut out = Vec::with_capacity(tw * trimmed.height as usize);
    for y in min_y..=max_y {
        out.extend_from_slice(&coverage[y * w + min_x..y * w + max_x + 1]);
    }
    Some((trimmed, out))
}

/// The winding number of `ring` about `point`, by the standard crossing count.
///
/// A ray is cast in +x. An edge counts once when it crosses the ray, signed by
/// whether it was going down or up. The half-open comparison (`<=` on one end,
/// `>` on the other) is what stops a vertex exactly on the ray being counted
/// twice — the classic off-by-one in this algorithm, and the one that makes a
/// rectangle's corner report as outside itself.
fn winding(ring: &[Vec2], point: Vec2) -> i32 {
    let mut w = 0;
    for i in 0..ring.len() {
        let a = ring[i];
        let b = ring[(i + 1) % ring.len()];
        if a.y <= point.y {
            if b.y > point.y && cross(a, b, point) > 0.0 {
                w += 1;
            }
        } else if b.y <= point.y && cross(a, b, point) < 0.0 {
            w -= 1;
        }
    }
    w
}

/// Which side of the line `a -> b` the point falls on. Positive is left.
fn cross(a: Vec2, b: Vec2, p: Vec2) -> f32 {
    (b.x - a.x) * (p.y - a.y) - (p.x - a.x) * (b.y - a.y)
}

/// Fill `rings` into an 8-bit coverage mask over `rect`.
///
/// Scanline, nonzero winding, [`SUB_SCANLINES`] sub-rows per pixel row with
/// exact horizontal coverage — see the module docs for why the two axes are
/// treated differently.
fn rasterise(rings: &[Vec<Vec2>], rect: PixelRect) -> Vec<u8> {
    let width = rect.width as usize;
    let mut out = vec![0u8; width * rect.height as usize];
    // Both reused across every sub-scanline of every row: this runs once per
    // selection, but a lasso is thousands of segments over thousands of rows
    // and a fresh allocation per sub-row would be millions of them.
    let mut acc = vec![0.0f32; width];
    let mut crossings: Vec<(f32, i32)> = Vec::new();

    let weight = 1.0 / SUB_SCANLINES as f32;
    for row in 0..rect.height {
        acc.fill(0.0);
        for sub in 0..SUB_SCANLINES {
            let sy = rect.y as f32 + row as f32 + (sub as f32 + 0.5) / SUB_SCANLINES as f32;
            crossings.clear();
            for ring in rings {
                for i in 0..ring.len() {
                    let a = ring[i];
                    let b = ring[(i + 1) % ring.len()];
                    // Half-open in y, so a vertex shared by two edges is
                    // crossed exactly once and a horizontal edge not at all.
                    let (lo, hi, dir) = if a.y < b.y { (a, b, 1) } else { (b, a, -1) };
                    if sy < lo.y || sy >= hi.y {
                        continue;
                    }
                    let t = (sy - lo.y) / (hi.y - lo.y);
                    crossings.push((lo.x + t * (hi.x - lo.x), dir));
                }
            }
            if crossings.len() < 2 {
                continue;
            }
            crossings.sort_by(|a, b| a.0.total_cmp(&b.0));

            let mut winding = 0;
            for pair in crossings.windows(2) {
                winding += pair[0].1;
                if winding != 0 {
                    add_span(&mut acc, rect.x as f32, pair[0].0, pair[1].0, weight);
                }
            }
        }

        let base = row as usize * width;
        for (i, a) in acc.iter().enumerate() {
            out[base + i] = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    out
}

/// Add the horizontal coverage of the document-space span `[x0, x1)` to `acc`,
/// whose first entry is the pixel at document x `origin`.
///
/// Exact: a span covering three tenths of a pixel adds three tenths.
fn add_span(acc: &mut [f32], origin: f32, x0: f32, x1: f32, weight: f32) {
    let a = (x0 - origin).max(0.0);
    let b = (x1 - origin).min(acc.len() as f32);
    if b <= a {
        return;
    }
    let first = a.floor() as usize;
    let last = (b.ceil() as usize).min(acc.len());
    for (i, cell) in acc.iter_mut().enumerate().take(last).skip(first) {
        let lo = a.max(i as f32);
        let hi = b.min(i as f32 + 1.0);
        if hi > lo {
            *cell += (hi - lo) * weight;
        }
    }
}

// ---------------------------------------------------------------------------
// Softening an edge
// ---------------------------------------------------------------------------

/// Soften `coverage` over `bounds` by `radius` document pixels, growing the
/// rectangle to hold the falloff and clamping it to the canvas.
///
/// Two separable box passes of half-width `radius / 2`, which is a tent of
/// half-width `radius`: coverage runs from full to nothing across a band twice
/// the radius wide, centred on the edge it was blurred from. The module docs
/// have the argument for the shape of the kernel; these are the two things the
/// code has to get right.
///
/// **The grown rectangle is a bound, not a measurement.** The discrete box has
/// a tap one beyond its own half-width carrying the fractional weight, so the
/// pair reaches `2 × (⌊radius / 2⌋ + 1)` — never less than the radius, and up
/// to a pixel more. Growing by exactly `radius` would clip the last of the
/// falloff to zero along a straight line, which is a visible ledge on a wide
/// feather. [`trim`] then takes the rectangle back to the pixels that actually
/// hold anything, so what a selection costs is measured rather than guessed.
///
/// **Everything outside the mask is zero**, including everything outside the
/// canvas, which is what makes a selection fade at the edge of the document.
///
/// **What it costs, said the way [`trace_rings`] says it.** Linear in the area
/// whatever the radius, but with a real constant: two byte buffers the size of
/// the grown rectangle, so a full-canvas feathered selection on a 2048²
/// document is about 8 MB of scratch and on a 10000² one about 200 MB, given
/// straight back. The vertical pass walks columns, which is a cache line per
/// access on a wide rectangle. Unlike `trace_rings` this runs on *every*
/// feathered gesture rather than only after a boolean — but still only at
/// pointer-up, on a flip, and at a transform commit. **Nothing on the drawing
/// path may reach it**, and nothing does: [`Selection::feathered`] is called
/// from `SelectionDraft::finish`, [`Selection::flipped`] and
/// `Editor::carry_selection`, and from nowhere else.
fn soften(
    bounds: PixelRect,
    coverage: &[u8],
    radius: f32,
    doc: UVec2,
) -> Option<(PixelRect, Vec<u8>)> {
    let half = radius * 0.5;
    let pad = 2 * (half.floor() as u32 + 1);
    let x0 = bounds.x.saturating_sub(pad);
    let y0 = bounds.y.saturating_sub(pad);
    let x1 = (bounds.x + bounds.width + pad).min(doc.x.max(bounds.x + bounds.width));
    let y1 = (bounds.y + bounds.height + pad).min(doc.y.max(bounds.y + bounds.height));
    let grown = PixelRect {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    };

    let w = grown.width as usize;
    let h = grown.height as usize;
    let src_w = bounds.width as usize;
    let inset_x = (bounds.x - grown.x) as usize;
    let inset_y = (bounds.y - grown.y) as usize;

    // **Two box passes per axis, not one for the pair.** A single pass along
    // each axis is a box in two dimensions, whose reach is `half` rather than
    // the radius and whose profile has a corner in it; the tent this wants is
    // the box convolved with itself, and convolution is per axis. Getting that
    // wrong is a feather a quarter of the width asked for, which looks like a
    // feather and is not the one on the control.
    //
    // The intermediate between the two axes is bytes rather than floats: it is
    // the whole grown rectangle, and a canvas-sized selection in `f32` would be
    // four times the mask it is softening. One store's worth of rounding is the
    // price, and it is symmetric — every partial sum is an exact integer, so
    // two equal inputs round to two equal outputs and a rectangle comes out
    // exactly mirrored about its own centre.
    let mut mid = vec![0u8; w * h];
    let span = w.max(h);
    let mut line = vec![0f32; span];
    let mut scratch = vec![0f32; span];

    for y in 0..h {
        line[..w].fill(0.0);
        if y >= inset_y && y - inset_y < bounds.height as usize {
            let s = (y - inset_y) * src_w;
            for (x, cell) in line[inset_x..inset_x + src_w].iter_mut().enumerate() {
                *cell = f32::from(coverage[s + x]);
            }
        }
        box_blur(&line[..w], &mut scratch[..w], half);
        box_blur(&scratch[..w], &mut line[..w], half);
        for (x, v) in line[..w].iter().enumerate() {
            mid[y * w + x] = to_byte(*v);
        }
    }

    let mut result = vec![0u8; w * h];
    for x in 0..w {
        for (y, cell) in line[..h].iter_mut().enumerate() {
            *cell = f32::from(mid[y * w + x]);
        }
        box_blur(&line[..h], &mut scratch[..h], half);
        box_blur(&scratch[..h], &mut line[..h], half);
        for (y, v) in line[..h].iter().enumerate() {
            result[y * w + x] = to_byte(*v);
        }
    }

    trim(grown, result)
}

/// Round a coverage in 0..=255 back into a byte.
fn to_byte(v: f32) -> u8 {
    v.clamp(0.0, 255.0).round() as u8
}

/// One box pass of half-width `half` over `src`, into `dst`.
///
/// Weights are 1 for every tap within `⌊half⌋` and the fraction for the one
/// beyond, normalised by `2 × half + 1` — so the kernel is continuous in the
/// radius and a rail dragged through it does not step. Off either end reads as
/// zero, which is what makes a selection fade at the edge of the canvas.
///
/// A running sum, so the cost is the length rather than the length times the
/// radius: a 250-pixel feather over a full-canvas selection is otherwise five
/// hundred multiply-adds per pixel per pass. The sum is only ever fed exact
/// integers and only ever added to and subtracted from, so it stays exact —
/// which is what makes the whole pass symmetric.
fn box_blur(src: &[f32], dst: &mut [f32], half: f32) {
    let n = src.len();
    if half <= 0.0 || n == 0 {
        dst[..n].copy_from_slice(src);
        return;
    }
    let k = half.floor() as isize;
    let frac = half - k as f32;
    let norm = 1.0 / (2.0 * half + 1.0);
    let at = |i: isize| -> f32 {
        if i < 0 || i >= n as isize {
            0.0
        } else {
            src[i as usize]
        }
    };

    let mut sum = 0.0f32;
    for j in -k..=k {
        sum += at(j);
    }
    for i in 0..n as isize {
        dst[i as usize] = (sum + frac * (at(i - k - 1) + at(i + k + 1))) * norm;
        sum += at(i + k + 1) - at(i - k);
    }
}

// ---------------------------------------------------------------------------
// Tracing an outline back out of a mask
// ---------------------------------------------------------------------------

/// A directed boundary edge, as stored in the two grids below.
const FORWARD: u8 = 1;
/// The same edge run the other way — see [`trace_rings`].
const BACKWARD: u8 = 2;

/// Trace the closed rings round the pixels of `coverage` that are at least
/// [`INSIDE`], in document space.
///
/// Only ever called after a boolean, and the module docs say what that costs.
/// It runs at pointer-up, once, and it is linear in the area — a full-canvas
/// selection on a 2048² document builds about 16 MB of grids and gives them
/// straight back, which is the same order as the rasterisation that produced
/// the mask a moment earlier. Nothing on the drawing path may reach it.
///
/// The method is boundary-edge walking, not marching squares over a 2×2 window:
/// it is the same information and it comes out already oriented. Every pixel
/// that is inside contributes one **directed** edge for each neighbour that is
/// outside, oriented so the inside is always on the *right* of the direction of
/// travel. Outer rings then come out wound the way [`Selection::rectangle`]
/// winds — the top edge running +x — and a ring round a hole comes out wound
/// the other way, so nonzero winding empties the hole with no special case and
/// [`Selection::contains`] keeps working on a traced outline.
///
/// Each vertex has at most two outgoing edges, and two only where two inside
/// pixels meet corner to corner across two outside ones. There the walk turns
/// as sharply as it can — the first candidate is a right turn — which keeps
/// diagonally touching regions apart rather than pinching them into one ring.
/// The choice is made against the *original* edges rather than the ones still
/// unwalked, so it pairs each arrival with exactly one departure and the rings
/// are the cycles of that pairing; a walk therefore ends by arriving back at
/// the edge it started from and can never wander onto another ring.
fn trace_rings(bounds: PixelRect, coverage: &[u8]) -> Vec<Vec<Vec2>> {
    let w = bounds.width as usize;
    let h = bounds.height as usize;
    let inside = |x: isize, y: isize| {
        x >= 0
            && y >= 0
            && (x as usize) < w
            && (y as usize) < h
            && coverage[y as usize * w + x as usize] >= INSIDE
    };

    // Horizontal edges live on the grid lines between pixel rows: `h + 1` rows
    // of `w`. Vertical ones on the lines between columns: `h` rows of `w + 1`.
    let mut hor = vec![0u8; (h + 1) * w];
    let mut ver = vec![0u8; h * (w + 1)];
    for gy in 0..=h {
        for gx in 0..w {
            hor[gy * w + gx] = match (
                inside(gx as isize, gy as isize - 1),
                inside(gx as isize, gy as isize),
            ) {
                // The top edge of an inside pixel, run +x: inside below, which
                // is the right-hand side when facing +x with y pointing down.
                (false, true) => FORWARD,
                // Its bottom edge, run -x, for the same reason mirrored.
                (true, false) => BACKWARD,
                _ => 0,
            };
        }
    }
    for gy in 0..h {
        for gx in 0..=w {
            ver[gy * (w + 1) + gx] = match (
                inside(gx as isize - 1, gy as isize),
                inside(gx as isize, gy as isize),
            ) {
                // The right edge of an inside pixel, run +y.
                (true, false) => FORWARD,
                // Its left edge, run -y.
                (false, true) => BACKWARD,
                _ => 0,
            };
        }
    }

    // An edge is named by its index into one grid or the other. Horizontal
    // indices come first so one number covers both.
    let split = hor.len();
    let dir_of = |e: usize| -> u8 {
        if e < split {
            // 0 is +x, 2 is -x.
            if hor[e] == FORWARD { 0 } else { 2 }
        } else if ver[e - split] == FORWARD {
            // 1 is +y, 3 is -y.
            1
        } else {
            3
        }
    };
    let ends_of = |e: usize| -> ((usize, usize), (usize, usize)) {
        if e < split {
            let (gx, gy) = (e % w, e / w);
            if hor[e] == FORWARD {
                ((gx, gy), (gx + 1, gy))
            } else {
                ((gx + 1, gy), (gx, gy))
            }
        } else {
            let i = e - split;
            let (gx, gy) = (i % (w + 1), i / (w + 1));
            if ver[i] == FORWARD {
                ((gx, gy), (gx, gy + 1))
            } else {
                ((gx, gy + 1), (gx, gy))
            }
        }
    };
    // The edge leaving `(gx, gy)` in direction `d`, if there is one.
    let leaving = |(gx, gy): (usize, usize), d: u8| -> Option<usize> {
        match d {
            0 if gx < w && hor[gy * w + gx] == FORWARD => Some(gy * w + gx),
            2 if gx > 0 && hor[gy * w + gx - 1] == BACKWARD => Some(gy * w + gx - 1),
            1 if gy < h && ver[gy * (w + 1) + gx] == FORWARD => Some(split + gy * (w + 1) + gx),
            3 if gy > 0 && ver[(gy - 1) * (w + 1) + gx] == BACKWARD => {
                Some(split + (gy - 1) * (w + 1) + gx)
            }
            _ => None,
        }
    };

    let mut walked = vec![false; hor.len() + ver.len()];
    let mut rings = Vec::new();
    let mut ring: Vec<Vec2> = Vec::new();
    for start in 0..walked.len() {
        let present = if start < split {
            hor[start] != 0
        } else {
            ver[start - split] != 0
        };
        if !present || walked[start] {
            continue;
        }
        ring.clear();
        let mut e = start;
        loop {
            walked[e] = true;
            let (from, to) = ends_of(e);
            ring.push(Vec2::new(
                bounds.x as f32 + from.0 as f32,
                bounds.y as f32 + from.1 as f32,
            ));
            let d = dir_of(e);
            // Right, straight on, left, back the way we came.
            let Some(next) = [1u8, 0, 3, 2]
                .into_iter()
                .find_map(|turn| leaving(to, (d + turn) % 4))
            else {
                break;
            };
            // Back at an edge already claimed — normally the one this ring
            // began at. Anything else would mean the pairing above was not a
            // pairing, and stopping is the safe reading either way.
            if walked[next] {
                break;
            }
            e = next;
        }
        // Only the corners are kept. A straight run along the edge of a large
        // selection is one segment however many pixels long it is, which is
        // what stops the outline being redrawn from thousands of points every
        // frame for the rest of the session.
        let corners = corners_only(&ring);
        if corners.len() >= 3 {
            rings.push(corners);
        }
    }
    rings
}

/// Drop the points of a closed ring that lie in the middle of a straight run.
fn corners_only(ring: &[Vec2]) -> Vec<Vec2> {
    let n = ring.len();
    if n < 3 {
        return ring.to_vec();
    }
    let mut out = Vec::new();
    for i in 0..n {
        let prev = ring[(i + n - 1) % n];
        let here = ring[i];
        let next = ring[(i + 1) % n];
        // Axis-aligned throughout, so "same direction" is a sign test and
        // needs no tolerance.
        let a = here - prev;
        let b = next - here;
        if a.x * b.y != a.y * b.x {
            out.push(here);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Curved outlines
// ---------------------------------------------------------------------------

/// The furthest a flattened arc may sit from the true curve, in document
/// pixels.
///
/// A twentieth of a pixel. The mask cannot see it — [`SUB_SCANLINES`] resolves
/// a quarter of a row and horizontal coverage is exact, so a deviation this far
/// under a pixel moves at most one byte by one level — and the *marquee* can
/// only see it once the camera is past 20:1, which is above the zoom anybody
/// draws a selection at. Tighter buys nothing anybody can look at and costs
/// vertices on a ring the outline walks every frame; looser is visible on a
/// large circle at 1:1.
const ARC_TOLERANCE: f32 = 0.05;

/// The most segments one quarter-turn is ever flattened into.
///
/// The count is `O(sqrt(radius))` — about `(π/4)·sqrt(r / 2t)` — so the largest
/// circle that fits `Document::MAX_EDGE`'s 32768 pixels has a **radius** of
/// 16384 and asks for 318 segments a quarter. This bound is never met by a
/// shape anybody can draw; it exists because the radius reaches [`arc_steps`]
/// from a drag in progress, and an infinity there would otherwise be a `Vec`
/// grown until the process died.
///
/// The figure was 450 here, which is the answer for a radius of 32768 — a
/// circle spanning twice the canvas ceiling. Left as a note rather than
/// silently corrected because it is the sort of number the next person sizing
/// this argues against: 318 leaves 60% of headroom where 450 suggests 14%.
const MAX_ARC_STEPS: u32 = 512;

/// How many straight segments a quarter-turn of radius `r` needs to stay within
/// [`ARC_TOLERANCE`].
///
/// The sagitta of a chord subtending `θ` on a circle of radius `r` is
/// `r (1 − cos(θ/2))`, so holding it under `t` needs
/// `θ ≤ 2 acos(1 − t/r)` and the count is `⌈(π/2) / θ⌉`. Derived rather than a
/// fixed 16-per-quarter, because a fixed count is either wasteful on a small
/// corner or visibly faceted on a large one, and a rounded rectangle draws four
/// arcs whose radius the artist sets with a rail.
///
/// A radius at or under the tolerance is a corner nobody can see, so it is one
/// segment.
fn arc_steps(r: f32) -> u32 {
    // Written as a NaN test and a comparison rather than a negated `>`, so
    // that the case a radius comes out of a drag as NaN is stated rather than
    // relying on how `!` reads over a partial order.
    if r.is_nan() || r <= ARC_TOLERANCE {
        return 1;
    }
    let theta = 2.0 * (1.0 - ARC_TOLERANCE / r).clamp(-1.0, 1.0).acos();
    if theta.is_nan() || theta <= 0.0 {
        return MAX_ARC_STEPS;
    }
    ((std::f32::consts::FRAC_PI_2 / theta).ceil() as u32).clamp(1, MAX_ARC_STEPS)
}

/// Append the **interior** of a quarter-turn about `centre` with radii `r`,
/// from angle `from` to `from + FRAC_PI_2`.
///
/// Both endpoints are left out, and that is not tidiness. `cos(π/2)` in `f32`
/// is `-4.4e-8` rather than zero, so an arc's own last point misses the corner
/// it lands on by about a millionth of a pixel — which is invisible in the mask
/// and is still a *second* vertex a hair from the one the straight run beside
/// it contributes. The caller pushes every junction analytically instead, so
/// the two agree bit for bit and [`push_new`] can drop the duplicate by exact
/// equality wherever a straight run has no length at all.
fn push_quarter(out: &mut Vec<Vec2>, centre: Vec2, r: Vec2, from: f32, steps: u32) {
    for k in 1..steps {
        let a = from + std::f32::consts::FRAC_PI_2 * (k as f32 / steps as f32);
        let (s, c) = a.sin_cos();
        out.push(centre + Vec2::new(c * r.x, s * r.y));
    }
}

/// The ring of an axis-aligned box with corners rounded by `roundness`.
///
/// Cleared first, and clockwise in document space (y down) so it winds the same
/// way [`Selection::rectangle`]'s four corners do at every roundness.
///
/// One function so that the outline drawn during the drag and the ring
/// [`Selection::rounded_rectangle`] rasterises at the end of it are the same
/// geometry rather than two implementations that have to agree — the rule
/// `render_float` keeps between a transform's preview and its commit.
fn rounded_rect_ring(a: Vec2, b: Vec2, roundness: f32, out: &mut Vec<Vec2>) {
    out.clear();
    let min = a.min(b);
    let max = a.max(b);
    let size = max - min;
    let r = 0.5 * size.min_element() * roundness.clamp(0.0, 1.0);
    // NaN reaches here from a roundness typed into the rail, and `clamp`
    // passes one straight through; a square is the safe answer for it.
    if r.is_nan() || r <= 0.0 {
        out.extend_from_slice(&[min, Vec2::new(max.x, min.y), max, Vec2::new(min.x, max.y)]);
        return;
    }
    let steps = arc_steps(r);
    let rr = Vec2::splat(r);
    use std::f32::consts::{FRAC_PI_2, PI};
    // Each straight run's far end, and then the corner it leads into as a
    // quarter-turn whose own first point is that end. Angles are measured with
    // y *down*, so the sweep that looks anticlockwise on paper is the clockwise
    // one here.
    //
    // The runs are pushed only where they are a run. At full roundness the
    // radius is half the *shorter* side, so both straight edges on that axis
    // have zero length — a fully rounded square has none at all — and pushing
    // them anyway would put coincident points in the ring. A zero-length edge
    // is one the rasteriser walks on every sub-scanline that spans it and one
    // more term in `winding`'s sum, for a segment that encloses nothing.
    push_new(out, Vec2::new(min.x + r, min.y));
    push_new(out, Vec2::new(max.x - r, min.y));
    push_quarter(out, Vec2::new(max.x - r, min.y + r), rr, -FRAC_PI_2, steps);
    push_new(out, Vec2::new(max.x, min.y + r));
    push_new(out, Vec2::new(max.x, max.y - r));
    push_quarter(out, Vec2::new(max.x - r, max.y - r), rr, 0.0, steps);
    push_new(out, Vec2::new(max.x - r, max.y));
    push_new(out, Vec2::new(min.x + r, max.y));
    push_quarter(out, Vec2::new(min.x + r, max.y - r), rr, FRAC_PI_2, steps);
    push_new(out, Vec2::new(min.x, max.y - r));
    push_new(out, Vec2::new(min.x, min.y + r));
    push_quarter(out, Vec2::new(min.x + r, min.y + r), rr, PI, steps);
    // The last corner lands back on the ring's first point, which the implicit
    // closing edge already carries — so nothing is pushed for it.
    //
    // [`push_new`] compares against the previous point only, which is enough
    // everywhere except across the ring's own seam: where `r` is under half an
    // ulp of the coordinate, `min.x + r == min.x` and the *last* point
    // collapses onto the *first*, leaving the zero-length closing edge this
    // whole arrangement exists to avoid. Reachable only at roundness about
    // 1e-5 near `Document::MAX_EDGE`, which is a hundred-thousandth of a
    // percentage rail and so not reachable through the interface at all — but
    // the seam is one comparison and a guard that says "never" should be able
    // to mean it.
    if out.len() > 1 && out.last() == out.first() {
        out.pop();
    }
}

/// Append `p` unless it is already the last point.
///
/// Exact equality rather than a tolerance, because every caller is pushing a
/// point built from the *same* expression the last one was — `max.y - r` twice,
/// say — so where they coincide they are bit-identical, and a tolerance here
/// would be a second, looser rule about what counts as a distinct vertex.
fn push_new(out: &mut Vec<Vec2>, p: Vec2) {
    if out.last() != Some(&p) {
        out.push(p);
    }
}

/// The ring of the ellipse inscribed in the box between two corners.
///
/// Cleared first, and clockwise in document space like [`rounded_rect_ring`]'s.
/// The segment count comes from the **longer** semi-axis, because that is where
/// the curve is flattest against its chords and therefore where the tolerance
/// binds.
fn ellipse_ring(a: Vec2, b: Vec2, out: &mut Vec<Vec2>) {
    out.clear();
    let min = a.min(b);
    let max = a.max(b);
    let r = 0.5 * (max - min);
    let centre = min + r;
    let steps = arc_steps(r.max_element());
    for k in 0..4 * steps {
        let angle = std::f32::consts::TAU * (k as f32 / (4 * steps) as f32);
        let (s, c) = angle.sin_cos();
        out.push(centre + Vec2::new(c * r.x, s * r.y));
    }
}

/// How many corner-cutting passes a polyline with these samples needs to reach
/// [`LASSO_SEGMENT`].
///
/// A pass halves every segment, so the count is `⌈log2(spacing / target)⌉`,
/// bounded by [`MAX_LASSO_SMOOTHING`] and by what [`MAX_LASSO_RING`] leaves.
/// **Adaptive rather than a fixed number of
/// passes**, and the reason is that the two ends of the zoom range want
/// opposite things: a lasso drawn at 0.1 has ten-pixel edges and needs four
/// passes to become a curve, while one drawn at 8:1 already has edges an eighth
/// of a pixel long and needs none at all — where a fixed count would multiply
/// the busiest ring anybody can produce by eight for a difference no mask and
/// no camera can show. Zero passes is the exact identity, which is what makes
/// the zoomed-in case cost nothing.
///
/// The **mean** spacing rather than the largest. One long edge is a jump the
/// pointer made — a fast flick, or a dropped frame — and it is a straight line
/// the hand really took rather than a staircase, so letting it drive the count
/// would subdivide a whole gesture on the strength of the one segment that
/// least needs it.
///
/// **What makes that safe is that corner-cutting is local.** [`cut_corners`]
/// moves a vertex by a quarter of its own two edges, so the mean decides *how
/// many* passes the gesture gets while each corner is only ever cut in
/// proportion to how far apart its own neighbours are. A hand that slows down
/// to turn a deliberate corner lays dense samples there, and that corner
/// survives a gesture whose mean spacing was set by the fast stretches either
/// side of it — which is also why this bites so little at 1:1, where a quick
/// hand can produce a mean of several pixels.
/// `a_corner_the_hand_slowed_down_for_survives_a_fast_gesture` is the guard.
///
/// It is re-derived as the gesture grows, so a lasso whose mean spacing crosses
/// a power of two mid-drag changes smoothness once, by one halving. That is
/// sub-pixel by construction — the crossing happens at the target — and it is
/// the price of the preview and the finished shape sharing one function rather
/// than the count being snapshotted at a press, where the first two samples
/// would decide it for the whole gesture.
fn smoothing_passes(points: &[Vec2]) -> u32 {
    if points.len() < 3 {
        return 0;
    }
    let travel: f32 = points.windows(2).map(|w| w[0].distance(w[1])).sum();
    let spacing = travel / (points.len() - 1) as f32;
    if spacing.is_nan() || spacing <= LASSO_SEGMENT {
        return 0;
    }
    let passes = (spacing / LASSO_SEGMENT).log2().ceil();
    let wanted = if passes.is_finite() {
        passes as u32
    } else {
        MAX_LASSO_SMOOTHING
    };
    // How many doublings the output can still afford. Integer throughout, so a
    // sample count already past the cap gives `1.ilog2()`, which is zero passes
    // and the exact identity — the smoothing switches itself off rather than
    // clamping to something arbitrary.
    let doublings = (MAX_LASSO_RING / points.len()).max(1).ilog2();
    wanted.min(MAX_LASSO_SMOOTHING).min(doublings)
}

/// One corner-cutting pass over an **open** polyline, in place.
///
/// Chaikin: every interior vertex is replaced by the points a quarter and three
/// quarters along its two edges, so each pass halves the turn at every corner
/// and the sequence converges on a quadratic B-spline. The ends are pinned,
/// which is what makes this the *open* form.
///
/// **Open rather than closed, and that is a decision about the gesture.** A
/// lasso's ring is closed by an implicit edge from where the hand stopped back
/// to where it started, and that edge is not something anybody drew — it is
/// where the pen came off the glass. Cutting the corners either side of it
/// would move the outline near the start and the end of the stroke away from
/// the pixels the artist was looking at when they made them. Pinning the ends
/// also means the preview and the finished shape share this function's answer
/// exactly, since the preview draws the same polyline without the closing edge.
///
/// In place, growing backwards, because this runs on the drawing path: the
/// outline is rebuilt every frame of a drag. A scratch buffer beside the
/// caller's would be a **fresh** allocation on every one of those frames; the
/// `resize` here reallocates only when the caller's own buffer is short, which
/// during a drag is `Vec`'s doubling and therefore amortised to nothing. Not
/// "no allocation" — that is stronger than what this does, and the first draft
/// of this comment claimed it.
fn cut_corners(points: &mut Vec<Vec2>) {
    let n = points.len();
    if n < 3 {
        return;
    }
    points.resize(2 * n, Vec2::ZERO);
    let last = points[n - 1];
    // Backwards, because the pair written for vertex `i` lands at `2i + 1` and
    // `2i + 2`, both strictly past `i` — so nothing yet to be read is ever
    // overwritten. Forwards would clobber `points[1]` before it was used.
    for i in (0..n - 1).rev() {
        let (from, to) = (points[i], points[i + 1]);
        points[2 * i + 1] = from.lerp(to, 0.25);
        points[2 * i + 2] = from.lerp(to, 0.75);
    }
    points[2 * n - 1] = last;
}

/// A selection being drawn: the gesture, before it becomes a [`Selection`].
///
/// Lives here rather than in the interface because what each mode does with a
/// press, a move and a release is a rule, not a drawing — and a rule is
/// testable without a window. `panels.rs` and `dock.rs` keep the same division.
#[derive(Clone, Debug)]
pub struct SelectionDraft {
    mode: SelectionMode,
    /// What this gesture will do to the selection standing. Snapshotted when
    /// the gesture *begins* and never read again — see
    /// [`SelectionDraft::combining`].
    op: SelectionOp,
    /// How far the shape's edge is softened, in document pixels. Snapshotted
    /// with the operation and for the same reason — see
    /// [`SelectionDraft::feathered`].
    feather: f32,
    /// How far the rectangle's corners are rounded, `0.0` through `1.0`.
    /// Snapshotted with the operation and the feather, and for the same
    /// reason — see [`SelectionDraft::rounded`].
    roundness: f32,
    /// How heavily the lasso damps the hand. Snapshotted with the rest.
    stabiliser: f32,
    /// The stabiliser's filter state: where the damped point stands after the
    /// samples so far. Equal to the raw pointer position while the stabiliser
    /// is off, which is what makes zero the exact identity.
    smoothed: Vec2,
    /// Rectangle and ellipse: the corner the drag started at. Lasso: every
    /// sampled point. Polygon: every vertex clicked so far.
    ///
    /// **The lasso's are the raw samples**, not the smoothed outline.
    /// [`SelectionDraft::lasso_ring`] is what turns them into the curve, and it
    /// runs on the way out rather than on the way in so that the preview and
    /// the finished shape are one answer rather than two.
    points: Vec<Vec2>,
    /// Where the pointer is now. For the rectangle and the ellipse this is the
    /// opposite corner; for the polygon it is the rubber-band end of the next
    /// edge.
    cursor: Vec2,
}

/// How short a freehand lasso's segments are subdivided towards, in document
/// pixels.
///
/// **This is what fixes a lasso drawn zoomed out and inspected zoomed in**, and
/// the mechanism is worth stating because "record the pointer more often" is
/// the obvious repair and is not available. At zoom 0.1 one screen pixel *is*
/// ten document pixels, so the finest polyline the pointer can describe already
/// has a vertex only every ten document pixels; there is nothing finer to
/// sample. Zoom back in afterwards and each of those edges is a long straight
/// line meeting the next at a right angle — the staircase the artist reported.
/// Subdividing is the only thing that can put a curve there, because the detail
/// the samples lack has to be *interpolated* rather than measured.
///
/// One document pixel, because that is the finest distinction the mask can
/// carry: [`rasterise`] resolves a quarter of a row and exact horizontal
/// coverage, so a vertex every pixel already describes every byte the shape can
/// produce. Below it only the marquee could tell the difference, and only above
/// the zoom the gesture was made at.
///
/// The two alternatives to corner-cutting, and why not:
/// - **Catmull-Rom through the samples** interpolates them, so it reproduces
///   the screen lattice as a wave instead of removing it — smooth, and still
///   half a screen pixel wrong at every sample — and it overshoots at a fast
///   corner, putting the outline outside anything the hand went round.
/// - **A symmetric (1, 2, 1) average in place** removes the lattice noise for
///   nothing and adds no vertices, so it leaves the facets exactly where they
///   were. It is the right instrument for the wrong half of the problem.
const LASSO_SEGMENT: f32 = 1.0;

/// The most corner-cutting passes a lasso's samples ever get.
///
/// Each pass doubles the ring. It binds below about zoom 0.125, where the
/// sample spacing is over eight document pixels: somebody lassoing at 0.05 gets
/// segments of two pixels rather than one, which is a much better outline than
/// the twenty-pixel edges they have now.
const MAX_LASSO_SMOOTHING: u32 = 3;

/// The most vertices a smoothed lasso ring may reach.
///
/// **[`MAX_LASSO_SMOOTHING`] alone does not bound anything**, and the first
/// draft of this claimed it did. A pass doubles the ring, so three passes is
/// eight times the *samples* — and the samples are bounded only by how far the
/// hand travelled on the glass, which a slow ten-second lasso takes to several
/// thousand and a minute of scrubbing to a hundred thousand. Eight times that
/// is a multiplier on two costs, not a number:
///
/// - **The marquee**, which `ui::selection_outline` draws as one line segment
///   per vertex, every frame, for as long as the selection stands.
/// - **[`rasterise`]**, which is `height × SUB_SCANLINES × edges` with no
///   active-edge table — so multiplying the edges by eight multiplies the
///   pointer-up cost by eight, and it does so exactly where the shape is
///   tallest, because the zoom that needs three passes is the zoom somebody
///   lassoes a whole large canvas at. That loop is quadratic and predates this;
///   what is new is a change that would have multiplied it.
///
/// So the cap is on the **output**. 16384 vertices is a ring describing a shape
/// four thousand pixels across at one vertex per pixel, which is past any
/// selection somebody draws by hand — an ordinary lasso is five hundred to two
/// thousand samples and smooths in full. What it does is switch the smoothing
/// off for a gesture whose ring is already enormous, where the samples are
/// dense in *screen* terms anyway and there is correspondingly little staircase
/// to remove. `a_very_long_lasso_is_not_subdivided_into_a_slow_frame` is the
/// guard, and it is what a mutation deleting the pass cap fails on too.
const MAX_LASSO_RING: usize = 16384;

impl SelectionDraft {
    /// The heaviest damping the lasso may be asked for.
    ///
    /// `Brush::MAX_STABILIZATION`'s figure and its argument: the filter is
    /// `1 - stabiliser` clamped to at least `0.02`, so even 1.0 converges on
    /// the pointer eventually — slowly enough to feel broken, which is the only
    /// thing this bound is for. Stated here rather than imported because a
    /// brush is not in this module's dependencies and a selection outline is
    /// not a stroke; the two happening to agree is a coincidence worth leaving
    /// visible rather than a shared constant worth inventing a home for.
    pub const MAX_STABILISER: f32 = 0.95;

    /// Begin at `at`, in document space.
    pub fn new(mode: SelectionMode, at: Vec2) -> Self {
        Self {
            mode,
            op: SelectionOp::Replace,
            feather: 0.0,
            roundness: 0.0,
            stabiliser: 0.0,
            smoothed: at,
            points: vec![at],
            cursor: at,
        }
    }

    /// Make this gesture add to or subtract from the selection already there.
    ///
    /// A separate step rather than a fourth argument to [`SelectionDraft::new`]
    /// because [`SelectionOp::Replace`] is what a gesture is unless something
    /// says otherwise, and every existing caller means exactly that.
    ///
    /// **The modifier is read once, when the gesture begins.** A hand lets go
    /// of Shift halfway through a lasso and a polygon spans several clicks;
    /// reading it again at the end would let a key tapped mid-drag change what
    /// a finished gesture turns out to have meant. Snapshotting is also what
    /// every other paint application does, and it is the same rule
    /// `Editor::start_stroke` follows for the stroke's style.
    #[must_use]
    pub fn combining(mut self, op: SelectionOp) -> Self {
        self.op = op;
        self
    }

    pub fn mode(&self) -> SelectionMode {
        self.mode
    }

    /// Soften the finished shape's edge by `radius` document pixels.
    ///
    /// Snapshotted at the start of the gesture, exactly as
    /// [`SelectionDraft::combining`]'s operation is and for the same reason: a
    /// polygon spans several clicks, and a rail dragged between two of them
    /// must not change what the gesture already under way turns out to have
    /// meant. It is also what makes the preview honest — the outline drawn
    /// while the shape is being dragged is the rings, and the rings are exactly
    /// where they will be however soft the mask ends up.
    #[must_use]
    pub fn feathered(mut self, radius: f32) -> Self {
        self.feather = radius;
        self
    }

    /// Round the finished rectangle's corners by `roundness`, `0.0` square
    /// through `1.0`.
    ///
    /// Snapshotted at the start of the gesture, exactly as the operation and
    /// the feather are and for the same reason — and unlike those two it is
    /// visible in the preview, because the rounding is geometry rather than a
    /// property of the mask.
    ///
    /// Ignored by every mode but [`SelectionMode::Rectangle`], which is
    /// [`SelectionMode::extra`]'s statement rather than this function's: a
    /// draft records what the strip was set to and the mode decides what to do
    /// with it, so nothing here has to be kept in step with what the strip
    /// happened to draw.
    #[must_use]
    pub fn rounded(mut self, roundness: f32) -> Self {
        self.roundness = roundness;
        self
    }

    /// Damp the hand by `stabiliser`, `0.0` off through
    /// [`SelectionDraft::MAX_STABILISER`].
    ///
    /// Snapshotted with the rest. Ignored by every mode but
    /// [`SelectionMode::Lasso`] — a rectangle and a polygon are defined by
    /// clicks, and a click somebody aimed is not something to filter.
    #[must_use]
    pub fn stabilised(mut self, stabiliser: f32) -> Self {
        self.stabiliser = stabiliser;
        self
    }

    pub fn op(&self) -> SelectionOp {
        self.op
    }

    pub fn feather(&self) -> f32 {
        self.feather
    }

    pub fn roundness(&self) -> f32 {
        self.roundness
    }

    pub fn stabiliser(&self) -> f32 {
        self.stabiliser
    }

    /// A press, after the first. Returns true when the shape is now closed and
    /// the draft should be finished.
    ///
    /// Only the polygon has anything to do with this: the other three modes are
    /// one press, a drag and a release.
    ///
    /// `close_within` is in document pixels, and is how a click back on the
    /// first vertex closes the shape. It comes from the caller because it is a
    /// screen distance divided by the zoom — a fixed document distance would be
    /// impossible to hit at 10% and impossible to avoid at 800%.
    pub fn press(&mut self, at: Vec2, close_within: f32) -> bool {
        self.cursor = at;
        if self.mode != SelectionMode::Polygon {
            return false;
        }
        if self.points.len() >= 3
            && self
                .points
                .first()
                .is_some_and(|first| first.distance(at) <= close_within)
        {
            return true;
        }
        self.points.push(at);
        false
    }

    /// The pointer moved. For the lasso this may record a point.
    ///
    /// `step` is the least distance between two recorded samples, in document
    /// pixels, and it comes from the caller for exactly the reason
    /// [`SelectionDraft::press`]'s `close_within` does: it is a **screen**
    /// distance divided by the zoom.
    ///
    /// It used to be a constant document pixel, and that was wrong at both
    /// ends. Zoomed in to 8:1 it threw away seven of every eight screen pixels
    /// the hand travelled, which is a polyline with eight-pixel facets on the
    /// very view somebody chose in order to be precise. Zoomed out to 0.1 it
    /// never fired at all, because one screen pixel is already ten document
    /// pixels — so it was a bound that bit hardest exactly where there was
    /// least to bound. A screen distance bounds the recorded shape by how far
    /// the hand actually moved on the glass, which is what the constant was
    /// reaching for: a pointer at 1000 Hz resting still still records nothing,
    /// and a pen reporting sub-pixel positions no longer records its own
    /// jitter.
    pub fn moved(&mut self, at: Vec2, step: f32) {
        self.cursor = at;
        if self.mode != SelectionMode::Lasso {
            return;
        }
        let at = self.damped(at);
        if self
            .points
            .last()
            .is_none_or(|last| last.distance(at) >= step)
        {
            self.points.push(at);
        }
    }

    /// One sample through the stabiliser.
    ///
    /// Exponential smoothing, the filter `StrokeBuilder::extend` runs on a
    /// stroke — the same instrument for the same complaint, so a hand that has
    /// learnt what the brush's rail does knows what this one does.
    ///
    /// **Zero is the exact identity, and it is a branch rather than an alpha of
    /// one.** `s + (a - s) * 1.0` is not `a` in floating point wherever `s` and
    /// `a` are far apart in magnitude, so an alpha of one would move the odd
    /// sample by an ulp and make "the default records what the pointer
    /// reported" a claim about rounding rather than about behaviour.
    /// `a_lasso_with_no_stabiliser_records_the_points_it_was_given` pins it.
    fn damped(&mut self, at: Vec2) -> Vec2 {
        if self.stabiliser <= 0.0 {
            self.smoothed = at;
            return at;
        }
        let alpha = (1.0 - self.stabiliser).clamp(0.02, 1.0);
        self.smoothed += (at - self.smoothed) * alpha;
        self.smoothed
    }

    /// A release. Returns true when the shape is complete.
    ///
    /// The polygon is the one mode a release does not finish: its gesture is a
    /// sequence of clicks, and ending it on the first button-up would make it
    /// a two-point line every time.
    pub fn release(&mut self, at: Vec2, step: f32) -> bool {
        self.moved(at, step);
        self.mode != SelectionMode::Polygon
    }

    /// True once the draft describes something that could be selected.
    ///
    /// A polygon with two vertices is a line, and a rectangle dragged nowhere
    /// is a point; neither is a selection, and both are what a stray click
    /// produces.
    ///
    /// Read off the **raw** samples for the lasso rather than the smoothed
    /// ring: the smoothing doubles the count twice, so asking it here would
    /// call a two-sample twitch closable.
    pub fn is_closable(&self) -> bool {
        match self.mode {
            SelectionMode::Rectangle | SelectionMode::Ellipse => {
                let a = self.points[0];
                (a.x - self.cursor.x).abs() >= 1.0 && (a.y - self.cursor.y).abs() >= 1.0
            }
            SelectionMode::Lasso => self.points.len() >= 3,
            SelectionMode::Polygon => self.points.len() >= 3,
        }
    }

    /// Whether the outline [`SelectionDraft::outline_into`] writes closes back
    /// on itself while the gesture is still in progress.
    ///
    /// A rectangle's four corners and an ellipse's ring *are* the shape at
    /// every instant of the drag, so both are drawn shut. A lasso mid-drag and
    /// a polygon two clicks in are paths, and drawing the edge back to the
    /// start would promise a shape the next moment is going to change.
    ///
    /// Here rather than in `ui.rs` because it is a statement about the gesture,
    /// and exhaustive rather than a `matches!` so that a fifth mode has to
    /// answer it.
    pub fn outline_closed(&self) -> bool {
        match self.mode {
            SelectionMode::Rectangle | SelectionMode::Ellipse => true,
            SelectionMode::Lasso | SelectionMode::Polygon => false,
        }
    }

    /// The lasso's samples, smoothed, into `out` — which is cleared first.
    ///
    /// **One statement of the smoothing, called by the preview and by the
    /// finish.** Two would be two things to keep in step about a curve, and the
    /// symptom of their drifting is the outline jumping the instant the pen
    /// comes off the glass — the same failure `composite.wgsl` and
    /// `commit.wgsl` share a file to avoid.
    fn lasso_ring(&self, out: &mut Vec<Vec2>) {
        out.clear();
        out.extend_from_slice(&self.points);
        for _ in 0..smoothing_passes(&self.points) {
            cut_corners(out);
        }
    }

    /// Write the ring the draft currently describes into `out`, which is
    /// cleared first.
    ///
    /// Takes the caller's buffer rather than returning one because the outline
    /// is redrawn every frame of the drag, and this is the only thing in the
    /// selection path that runs per frame.
    pub fn outline_into(&self, out: &mut Vec<Vec2>) {
        out.clear();
        match self.mode {
            SelectionMode::Rectangle => {
                rounded_rect_ring(self.points[0], self.cursor, self.roundness, out);
            }
            SelectionMode::Ellipse => ellipse_ring(self.points[0], self.cursor, out),
            SelectionMode::Lasso => self.lasso_ring(out),
            // The rubber band is part of what the user is looking at: without
            // it the shape appears to lag one click behind the pointer.
            SelectionMode::Polygon => {
                out.extend_from_slice(&self.points);
                out.push(self.cursor);
            }
        }
    }

    /// Turn the draft into a selection, or `None` if it encloses nothing.
    ///
    /// The shape is rasterised sharp and then softened, never rasterised soft:
    /// [`Selection::from_rings`] stays the one rasteriser, and the feather is
    /// one pass over the mask it produced.
    pub fn finish(&self, doc: UVec2) -> Option<Selection> {
        let sharp = match self.mode {
            SelectionMode::Rectangle => {
                Selection::rounded_rectangle(self.points[0], self.cursor, self.roundness, doc)
            }
            SelectionMode::Ellipse => Selection::ellipse(self.points[0], self.cursor, doc),
            SelectionMode::Lasso => {
                // A fresh buffer, once, at pointer-up: `outline_into`'s
                // argument for taking the caller's is about the frame loop,
                // and this is not on it.
                let mut ring = Vec::new();
                self.lasso_ring(&mut ring);
                Selection::polygon(&ring, doc)
            }
            SelectionMode::Polygon => Selection::polygon(&self.points, doc),
        };
        sharp?.feathered(self.feather, doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::vec2;

    const DOC: UVec2 = UVec2::splat(64);

    /// The lasso step these tests hand `moved`.
    ///
    /// One document pixel, which is what one screen pixel is at zoom 1 — the
    /// zoom every test here is implicitly at, since none of them has a
    /// camera. Named rather than typed at each call so that a test about the
    /// step says so by passing something else.
    const STEP: f32 = 1.0;

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Selection {
        Selection::rectangle(vec2(x0, y0), vec2(x1, y1), DOC).expect("a rectangle")
    }

    // --- modes ------------------------------------------------------------

    /// `ALL` holds every mode, at the position an exhaustive match gives it.
    ///
    /// Walking `ALL` could only ever check what is in it, so the arms are the
    /// authority and the array is what is checked: a variant added without a
    /// row here is a compile error, and one filed in the wrong place is a
    /// failure naming it. `icons::every_icon_is_in_all_where_the_match_puts_it`
    /// is the same guard on the same hazard.
    #[test]
    fn every_mode_is_in_all_where_the_match_puts_it() {
        let at = |mode: SelectionMode| -> usize {
            match mode {
                SelectionMode::Rectangle => 0,
                SelectionMode::Ellipse => 1,
                SelectionMode::Lasso => 2,
                SelectionMode::Polygon => 3,
            }
        };
        for mode in SelectionMode::ALL {
            assert_eq!(
                SelectionMode::ALL[at(mode)],
                mode,
                "{mode:?} is filed elsewhere"
            );
        }
    }

    #[test]
    fn every_mode_says_what_its_gesture_is_and_which_setting_it_owns() {
        // A label and a hint are what the picker and the strip draw, so an
        // empty one is a row with nothing on it — and the two settings must be
        // claimed by exactly one mode each, or the strip either draws a rail
        // twice or offers one the gesture ignores.
        let mut owners = Vec::new();
        for mode in SelectionMode::ALL {
            assert!(!mode.label().is_empty(), "{mode:?} has no name");
            assert!(
                !mode.hint().is_empty(),
                "{mode:?} says nothing about itself"
            );
            if let Some(setting) = mode.extra() {
                owners.push(setting);
            }
        }
        owners.sort_by_key(|s| format!("{s:?}"));
        assert_eq!(
            owners,
            vec![ModeSetting::Roundness, ModeSetting::Stabiliser]
        );
    }

    // --- rounded rectangles and ellipses ----------------------------------

    /// How many pixels of the canvas the selection covers, counting a partly
    /// covered one by its coverage. The shapes below are about *area*, so this
    /// is what they measure rather than a vertex or a sampled pixel.
    fn area(s: &Selection) -> f32 {
        s.coverage().iter().map(|c| f32::from(*c) / 255.0).sum()
    }

    #[test]
    fn no_roundness_is_the_exact_identity() {
        // The same rule the feather's zero and the grain's `mix(1.0, tile, s)`
        // hold to. It matters more here than it reads: an arc of radius zero
        // still emits its endpoints, so a rounding path taken at zero would
        // hand the rasteriser eight vertices in coincident pairs where the
        // plain rectangle hands it four — and the plain rectangle is the one
        // shape the fill rule is exact on both axes for.
        let plain = rect(10.0, 12.0, 30.0, 24.0);
        for roundness in [0.0, -1.0, f32::NAN] {
            let rounded =
                Selection::rounded_rectangle(vec2(10.0, 12.0), vec2(30.0, 24.0), roundness, DOC)
                    .expect("a rectangle");
            assert_eq!(rounded.rings(), plain.rings(), "roundness {roundness}");
            assert_eq!(rounded.bounds(), plain.bounds(), "roundness {roundness}");
            assert_eq!(
                rounded.coverage(),
                plain.coverage(),
                "roundness {roundness}"
            );
        }
    }

    #[test]
    fn a_fully_rounded_square_is_a_disc() {
        // Roundness 1 takes the corner radius to half the shorter side, so on a
        // square there are no straight flanks left at all.
        let s = Selection::rounded_rectangle(vec2(8.0, 8.0), vec2(48.0, 48.0), 1.0, DOC)
            .expect("a disc");
        assert_eq!(
            s.bounds(),
            PixelRect {
                x: 8,
                y: 8,
                width: 40,
                height: 40
            }
        );
        // The corners of the box are outside it and the middles of its sides
        // are on the boundary.
        assert_eq!(
            s.coverage_at(8, 8),
            0,
            "the box's corner is inside the disc"
        );
        assert_eq!(s.coverage_at(28, 28), 255, "the middle is not selected");
        assert!(
            s.contains(vec2(28.0, 9.0)),
            "the top of the disc is missing"
        );

        // And it really is a circle rather than a rounded square: within half a
        // percent of πr², where the square it came from is 27% larger. The
        // slack is the fill rule's, not the geometry's — `SUB_SCANLINES`
        // resolves a quarter of a row, which under-reports a curve by a
        // fraction of a pixel per row, so the measured 1253.3 against 1256.6 is
        // the rasteriser and would not shrink if the arcs were exact.
        let r = 20.0;
        let disc = std::f32::consts::PI * r * r;
        let got = area(&s);
        assert!(
            (got - disc).abs() < disc * 0.005,
            "a fully rounded square covers {got:.1} px against a disc's {disc:.1}"
        );
    }

    #[test]
    fn a_fully_rounded_oblong_is_a_stadium_and_an_ellipse_is_not() {
        // The two shapes the picker has to be able to tell apart, on the one
        // box where they differ most: twice as wide as it is tall. A stadium
        // keeps its full height along the whole of the straight flank; an
        // ellipse is at its full height only where it crosses the centre.
        let (a, b) = (vec2(4.0, 20.0), vec2(44.0, 40.0));
        let stadium = Selection::rounded_rectangle(a, b, 1.0, DOC).expect("a stadium");
        let ellipse = Selection::ellipse(a, b, DOC).expect("an ellipse");
        assert_eq!(stadium.bounds(), ellipse.bounds(), "the same box");

        // A quarter of the way along the top edge, which is inside the
        // stadium's flat flank and well outside the ellipse.
        let probe = vec2(24.0 - 10.0, 20.5);
        assert!(stadium.contains(probe), "the stadium lost its flat top");
        assert!(!ellipse.contains(probe), "the ellipse has a flat top");

        // And by area: a stadium of these proportions is a 20x20 square plus a
        // disc of radius 10, which is 14% more than the ellipse's πab. Half a
        // percent of slack, for `a_fully_rounded_square_is_a_disc`'s reason.
        let disc = std::f32::consts::PI * 100.0;
        let expected_stadium = 400.0 + disc;
        let expected_ellipse = std::f32::consts::PI * 20.0 * 10.0;
        assert!(
            (area(&stadium) - expected_stadium).abs() < expected_stadium * 0.005,
            "the stadium covers {:.1} against {expected_stadium:.1}",
            area(&stadium)
        );
        assert!(
            (area(&ellipse) - expected_ellipse).abs() < expected_ellipse * 0.005,
            "the ellipse covers {:.1} against {expected_ellipse:.1}",
            area(&ellipse)
        );
    }

    #[test]
    fn a_curved_outline_is_flattened_finely_enough_to_be_a_curve() {
        // **The quantity here is the sagitta, and the vertices are exactly the
        // wrong thing to read.** `ellipse_ring` puts every vertex on the curve
        // by construction — it *is* `centre + (cos, sin) · r` — so a reading of
        // how far a vertex sits from the circle is f32 noise whatever
        // `arc_steps` answers, and it gets **better** as the flattening gets
        // coarser. The first draft of this test did that, and measured: with
        // `arc_steps` mutated to 1 a circle of radius 900 becomes a square 264
        // pixels off the curve, and the vertex reading came back 0.000000
        // against the shipped code's 0.000122. It passed. `ARC_TOLERANCE` is a
        // bound on the deviation *between* two vertices, so that is what has to
        // be measured: the midpoint of each chord.
        let big = UVec2::splat(2048);
        for radius in [8.0f32, 100.0, 900.0] {
            let centre = Vec2::splat(1000.0);
            let s = Selection::ellipse(centre - radius, centre + radius, big).expect("a disc");
            let ring = &s.rings()[0];
            let worst = worst_sagitta(ring, centre, radius);
            assert!(
                worst <= ARC_TOLERANCE,
                "a circle of radius {radius} bows {worst:.4} px away from the true \
                 curve between two of its {} vertices, past the {ARC_TOLERANCE} it \
                 promises",
                ring.len()
            );
            // And it is not simply flattening everything to death: the count
            // follows the square root of the radius rather than the radius.
            assert!(
                ring.len() < 4 * MAX_ARC_STEPS as usize,
                "a circle of radius {radius} took {} vertices",
                ring.len()
            );
        }
    }

    #[test]
    fn a_curved_ring_never_repeats_a_point() {
        // A ring's closing edge is implicit, so a first point emitted again at
        // the end is a zero-length edge the rasteriser walks on every
        // sub-scanline that spans it — and `winding` counts it. Both curved
        // constructors join arcs to straight runs, which is exactly where a
        // duplicate creeps in.
        let mut ring = Vec::new();
        for roundness in [0.2f32, 0.5, 1.0] {
            rounded_rect_ring(vec2(4.0, 4.0), vec2(40.0, 28.0), roundness, &mut ring);
            for (i, w) in ring.windows(2).enumerate() {
                assert!(
                    w[0].distance(w[1]) > 1e-4,
                    "roundness {roundness} repeats a point at {i}"
                );
            }
            assert!(
                ring[0].distance(*ring.last().expect("a ring")) > 1e-4,
                "roundness {roundness} closes the ring by hand"
            );
        }
        ellipse_ring(vec2(4.0, 4.0), vec2(40.0, 28.0), &mut ring);
        assert!(ring[0].distance(*ring.last().expect("a ring")) > 1e-4);
    }

    #[test]
    fn a_whole_pixel_rectangle_is_exactly_covered() {
        // The commonest selection there is, and the one case where every
        // sub-scanline and every span end lands on an integer. Anything less
        // than 0 outside and 255 inside would mean the rasteriser's idea of
        // where a pixel is disagrees with the document's.
        let s = rect(10.0, 10.0, 20.0, 20.0);
        assert_eq!(
            s.bounds(),
            PixelRect {
                x: 10,
                y: 10,
                width: 10,
                height: 10
            }
        );
        assert_eq!(s.coverage_at(10, 10), 255);
        assert_eq!(s.coverage_at(19, 19), 255);
        assert_eq!(s.coverage_at(9, 15), 0);
        assert_eq!(s.coverage_at(20, 15), 0);
    }

    #[test]
    fn corners_given_in_any_order_select_the_same_box() {
        let a = rect(10.0, 10.0, 20.0, 20.0);
        let b = rect(20.0, 20.0, 10.0, 10.0);
        assert_eq!(a.bounds(), b.bounds());
        assert_eq!(a.coverage(), b.coverage());
    }

    #[test]
    fn a_half_covered_pixel_is_half_selected() {
        // The edge falls down the middle of column 20, so that column is half
        // in. This is the whole reason coverage is a byte rather than a bit:
        // without it a selection edge is a staircase.
        let s = rect(10.0, 10.0, 20.5, 20.0);
        let half = s.coverage_at(20, 15);
        assert!(
            (120..=136).contains(&half),
            "expected ~128 for a half-covered pixel, got {half}"
        );
        assert_eq!(s.coverage_at(19, 15), 255);
    }

    #[test]
    fn a_triangle_ramps_across_its_diagonal() {
        // Coverage on the diagonal has to be somewhere between the two sides,
        // which only holds if the sub-scanlines and the span ends are both
        // doing their job. The hypotenuse runs x + y = 40, so pixel (19, 20)
        // straddles it and the two corners do not.
        let s = Selection::polygon(&[vec2(10.0, 10.0), vec2(30.0, 10.0), vec2(10.0, 30.0)], DOC)
            .expect("a triangle");
        assert_eq!(s.coverage_at(11, 11), 255, "well inside");
        assert_eq!(s.coverage_at(29, 29), 0, "well outside");
        let edge = s.coverage_at(19, 20);
        assert!(
            (1..=254).contains(&edge),
            "the diagonal should be partly covered, got {edge}"
        );
    }

    #[test]
    fn overlapping_rings_stay_selected_rather_than_cancelling() {
        // Nonzero winding, not even-odd. Two rings wound the same way, one
        // inside the other: nonzero fills the middle, even-odd punches a hole
        // in it. That difference is what a freehand lasso crossing its own path
        // runs into, and a hole where the artist drew a loop is not what they
        // asked for.
        let outer = vec![
            vec2(10.0, 10.0),
            vec2(40.0, 10.0),
            vec2(40.0, 40.0),
            vec2(10.0, 40.0),
        ];
        let inner = vec![
            vec2(20.0, 20.0),
            vec2(30.0, 20.0),
            vec2(30.0, 30.0),
            vec2(20.0, 30.0),
        ];
        let s = Selection::from_rings(vec![outer, inner], DOC).expect("two rings");
        assert_eq!(s.coverage_at(25, 25), 255, "the overlap must stay selected");
        assert!(s.contains(vec2(25.0, 25.0)), "and the outline agrees");
    }

    #[test]
    fn a_point_on_the_boundary_is_counted_once() {
        // The classic failure of a crossing count: a vertex exactly on the ray
        // counted by both of its edges, which reports the inside of a rectangle
        // as outside along one row.
        //
        // The rule is half-open, matching the mask: the top and left edges
        // belong to the selection and the bottom and right ones to whatever is
        // beyond it, so two selections sharing an edge do not both claim it.
        let s = rect(10.0, 10.0, 20.0, 20.0);
        assert!(s.contains(vec2(15.0, 10.0)), "the top edge is inside");
        assert!(s.contains(vec2(10.0, 15.0)), "and so is the left one");
        assert!(!s.contains(vec2(15.0, 20.0)), "the bottom edge is not");
        assert!(s.contains(vec2(15.0, 15.0)));
        assert!(!s.contains(vec2(15.0, 25.0)));
        assert!(!s.contains(vec2(5.0, 15.0)));
    }

    #[test]
    fn a_selection_off_the_canvas_is_no_selection() {
        assert!(Selection::rectangle(vec2(-40.0, -40.0), vec2(-10.0, -10.0), DOC).is_none());
        // And one thinner than a pixel encloses nothing, however long it is.
        assert!(Selection::rectangle(vec2(10.0, 10.0), vec2(10.0, 40.0), DOC).is_none());
    }

    #[test]
    fn a_selection_is_clipped_to_the_canvas() {
        // Nothing downstream may be handed a rectangle that runs off the
        // texture: `write_texture` and `read_layer_rect` both refuse one, and
        // the failure is a validation error that takes the process with it.
        let s = rect(-20.0, -20.0, 30.0, 30.0);
        let b = s.bounds();
        assert_eq!((b.x, b.y), (0, 0));
        assert_eq!((b.width, b.height), (30, 30));
        assert_eq!(s.coverage().len(), 900);
    }

    #[test]
    fn a_rectangle_draft_needs_a_drag_before_it_encloses_anything() {
        let mut draft = SelectionDraft::new(SelectionMode::Rectangle, vec2(10.0, 10.0));
        assert!(!draft.is_closable(), "a click is not a selection");
        assert!(draft.release(vec2(10.2, 10.2), STEP));
        assert!(!draft.is_closable());
        draft.moved(vec2(30.0, 30.0), STEP);
        assert!(draft.is_closable());
        assert!(draft.finish(DOC).is_some());
    }

    #[test]
    fn a_polygon_closes_on_a_click_back_at_its_first_vertex() {
        let mut draft = SelectionDraft::new(SelectionMode::Polygon, vec2(10.0, 10.0));
        assert!(
            !draft.release(vec2(10.0, 10.0), STEP),
            "a release is not a close"
        );
        assert!(!draft.press(vec2(30.0, 10.0), 4.0));
        assert!(!draft.press(vec2(30.0, 30.0), 4.0));
        // Near the first vertex but not on it, which is what a real click is.
        assert!(draft.press(vec2(11.0, 12.0), 4.0));
        let s = draft.finish(DOC).expect("a triangle");
        assert_eq!(s.coverage_at(25, 15), 255);
    }

    #[test]
    fn a_polygon_does_not_close_before_it_is_a_shape() {
        // Two vertices and a click back on the start is a line, and closing on
        // it would leave the tool apparently dead: the selection would be
        // nothing and the draft would be gone.
        let mut draft = SelectionDraft::new(SelectionMode::Polygon, vec2(10.0, 10.0));
        assert!(!draft.press(vec2(30.0, 10.0), 4.0));
        assert!(!draft.press(vec2(10.5, 10.5), 4.0));
    }

    #[test]
    fn a_lasso_drops_samples_that_say_nothing() {
        // A pointer reports far faster than a document pixel changes. Every
        // recorded point is an edge the rasteriser walks on every sub-scanline
        // it spans, so the ones that repeat a position are dropped.
        let mut draft = SelectionDraft::new(SelectionMode::Lasso, vec2(10.0, 10.0));
        for _ in 0..100 {
            draft.moved(vec2(10.05, 10.05), STEP);
        }
        draft.moved(vec2(40.0, 10.0), STEP);
        draft.moved(vec2(40.0, 40.0), STEP);
        assert_eq!(draft.finish(DOC).map(|s| s.bounds().width), Some(30));
    }

    // --- the lasso's smoothing --------------------------------------------

    /// A circle sampled on a lattice `spacing` document pixels apart, which is
    /// what the pointer can describe at a zoom of `1 / spacing`.
    ///
    /// The snap is the whole fixture: it is *why* a lasso drawn zoomed out
    /// comes back as a staircase, and no amount of sampling the circle more
    /// finely can undo it. Consecutive duplicates are dropped because the
    /// pointer reports a position rather than a change.
    fn lattice_circle(centre: Vec2, radius: f32, spacing: f32) -> Vec<Vec2> {
        let mut out: Vec<Vec2> = Vec::new();
        for k in 0..4000 {
            let a = std::f32::consts::TAU * (k as f32 / 4000.0);
            let (s, c) = a.sin_cos();
            let p = centre + vec2(c * radius, s * radius);
            let snapped = vec2(
                (p.x / spacing).round() * spacing,
                (p.y / spacing).round() * spacing,
            );
            if out.last().is_none_or(|l| *l != snapped) {
                out.push(snapped);
            }
        }
        out
    }

    /// The sharpest corner in a polyline, in degrees. Zero is a straight line.
    fn sharpest_turn(points: &[Vec2]) -> f32 {
        let mut worst = 0.0f32;
        for w in points.windows(3) {
            let a = (w[1] - w[0]).normalize_or_zero();
            let b = (w[2] - w[1]).normalize_or_zero();
            worst = worst.max(a.dot(b).clamp(-1.0, 1.0).acos().to_degrees());
        }
        worst
    }

    fn longest_edge(points: &[Vec2]) -> f32 {
        points
            .windows(2)
            .map(|w| w[0].distance(w[1]))
            .fold(0.0f32, f32::max)
    }

    /// The furthest any vertex sits from the circle it was meant to trace.
    ///
    /// Right for a *sampled* polyline, where the samples are the thing under
    /// test — a lasso's lattice points really do sit off the curve. Wrong for a
    /// flattened arc, where the vertices are on the curve by construction; use
    /// [`worst_sagitta`] there.
    fn worst_radial_error(points: &[Vec2], centre: Vec2, radius: f32) -> f32 {
        points
            .iter()
            .map(|p| ((*p - centre).length() - radius).abs())
            .fold(0.0f32, f32::max)
    }

    /// The furthest a **chord** of a closed ring bows away from the circle it
    /// approximates, which is what a flattening tolerance is a bound on.
    ///
    /// Read at the chord's midpoint, where a circular arc's deviation is
    /// greatest. Cyclic, so the closing edge is measured like every other.
    fn worst_sagitta(ring: &[Vec2], centre: Vec2, radius: f32) -> f32 {
        (0..ring.len())
            .map(|i| {
                let mid = (ring[i] + ring[(i + 1) % ring.len()]) * 0.5;
                ((mid - centre).length() - radius).abs()
            })
            .fold(0.0f32, f32::max)
    }

    #[test]
    fn a_lasso_drawn_zoomed_out_comes_back_as_a_curve_and_not_a_staircase() {
        // The artist's report, in the smallest form that reproduces it: a
        // circle drawn at a tenth scale, where one screen pixel is ten document
        // pixels and the samples therefore land on a ten-pixel lattice.
        //
        // The whole polyline is measured rather than a vertex or two, because
        // "choppy" is a statement about the sharpest corner anywhere in it —
        // and both readings are taken from the *raw* samples too, so this fails
        // if the smoothing is removed and cannot pass by agreeing with itself
        // about a threshold.
        let centre = vec2(256.0, 256.0);
        let radius = 200.0;
        let spacing = 10.0;
        let raw = lattice_circle(centre, radius, spacing);

        let mut draft = SelectionDraft::new(SelectionMode::Lasso, raw[0]);
        for p in &raw[1..] {
            draft.moved(*p, spacing);
        }
        let mut ring = Vec::new();
        draft.lasso_ring(&mut ring);

        // A lattice staircase turns a right angle at its worst corner, whatever
        // the shape being traced, because the samples only ever step along an
        // axis or diagonally.
        let raw_turn = sharpest_turn(&raw);
        assert!(
            raw_turn > 85.0,
            "the fixture is not a staircase: its sharpest corner is {raw_turn:.1}°"
        );
        let smooth_turn = sharpest_turn(&ring);
        assert!(
            smooth_turn < 20.0,
            "the smoothed outline still corners at {smooth_turn:.1}°"
        );

        // And the facets are gone as well as the corners: a right angle spread
        // over segments still fourteen pixels long is a smooth polygon, not a
        // curve.
        let raw_edge = longest_edge(&raw);
        let smooth_edge = longest_edge(&ring);
        assert!(
            raw_edge >= spacing,
            "the fixture's longest edge is {raw_edge:.1} px, so its samples are \
             not a lattice apart"
        );
        assert!(
            smooth_edge < 2.0,
            "the smoothed outline still has {smooth_edge:.1} px facets"
        );

        // Corner-cutting moves the outline, so this says which way: **towards**
        // the shape the hand was tracing, because a lattice sample is always
        // outside or inside the true curve and the cut lands between two of
        // them. A smoother outline further from what was drawn would be a worse
        // selection wearing a better one's numbers.
        let raw_error = worst_radial_error(&raw, centre, radius);
        let smooth_error = worst_radial_error(&ring, centre, radius);
        assert!(
            smooth_error <= raw_error,
            "smoothing moved the outline away from the circle: {smooth_error:.2} \
             px against the samples' own {raw_error:.2}"
        );
    }

    #[test]
    fn a_lasso_drawn_zoomed_in_is_recorded_and_left_alone() {
        // The other end of the same rule, and the one that keeps the cost off
        // the frame loop. At 8:1 a screen pixel is an eighth of a document
        // pixel, so the samples are already finer than any byte of the mask can
        // express and there is nothing to interpolate: the ring is the samples,
        // vertex for vertex.
        let step = 0.125;
        let raw = lattice_circle(vec2(32.0, 32.0), 20.0, step);
        let mut draft = SelectionDraft::new(SelectionMode::Lasso, raw[0]);
        for p in &raw[1..] {
            draft.moved(*p, step);
        }
        let mut ring = Vec::new();
        draft.lasso_ring(&mut ring);
        assert_eq!(
            ring, raw,
            "a lasso finer than a pixel was subdivided anyway"
        );

        // And the step is what let those samples be recorded at all: the
        // constant document pixel this used to be would have thrown most of
        // them away. Counted rather than measured off the ring, because a
        // coarser polyline gets *more* smoothing passes and the two effects
        // would cancel in any reading of the finished geometry.
        let coarse = {
            let mut d = SelectionDraft::new(SelectionMode::Lasso, raw[0]);
            for p in &raw[1..] {
                d.moved(*p, 1.0);
            }
            d
        };
        assert!(
            draft.points.len() > 4 * coarse.points.len(),
            "a screen-pixel step recorded {} samples where a document-pixel one \
             recorded {}, which is not the difference this is about",
            draft.points.len(),
            coarse.points.len()
        );
    }

    #[test]
    fn a_corner_the_hand_slowed_down_for_survives_a_fast_gesture() {
        // **The zoom nothing else here drives, and the property that makes the
        // mean safe.** At 1:1 a quick hand reports several document pixels
        // apart, so `smoothing_passes` asks for two — where before this existed
        // the ring was the samples verbatim. That is a behaviour change at the
        // commonest zoom of all, and what stops it costing anybody a corner is
        // that corner-cutting is *local*: a vertex moves by a quarter of its
        // own two edges, so a corner somebody slowed down to draw is barely cut
        // even when the gesture's mean was set by the fast stretches around it.
        //
        // Two long fast legs meeting at a corner traced slowly, which is what a
        // hand actually does when it goes round something square.
        let corner = vec2(200.0, 20.0);
        let mut draft = SelectionDraft::new(SelectionMode::Lasso, vec2(20.0, 20.0));
        // In at six pixels a report, which is a fast hand at 1:1.
        for k in 1..=29 {
            draft.moved(vec2(20.0, 20.0).lerp(corner, k as f32 / 30.0), 1.0);
        }
        // Round the corner at a fifth of a pixel a report: the hand stopped.
        for k in 1..=10 {
            draft.moved(corner + vec2(-2.0 + k as f32 * 0.2, 0.0), 1.0);
        }
        for k in 1..=10 {
            draft.moved(corner + vec2(0.0, k as f32 * 0.2), 1.0);
        }
        // And away again, fast.
        for k in 1..=30 {
            draft.moved(corner.lerp(vec2(200.0, 200.0), k as f32 / 30.0), 1.0);
        }

        assert!(
            smoothing_passes(&draft.points) > 0,
            "the fixture's mean spacing did not ask for any smoothing, so this \
             says nothing about what smoothing does to a corner"
        );
        let mut ring = Vec::new();
        draft.lasso_ring(&mut ring);
        let nearest = ring
            .iter()
            .map(|p| p.distance(corner))
            .fold(f32::MAX, f32::min);
        assert!(
            nearest < 0.5,
            "the outline was rounded {nearest:.2} px away from a corner the hand \
             slowed right down to draw"
        );

        // **What this catches, demonstrated rather than claimed.** Mutating
        // `cut_corners` to cut at 0.45/0.55 instead of 0.25/0.75 leaves it
        // green, and so does a fixed three-pixel cut clamped to half an edge.
        // That is not a gap; it is the property itself, because any cut
        // expressed as a fraction of a vertex's *own* edges is invisible
        // where those edges are a fifth of a pixel. What does fail it is a
        // smoother whose reach is the gesture's rather than the edge's:
        // resampling the finished ring at a uniform three pixels rounds this
        // corner 0.81 px off. That is the change this exists to refuse, since
        // it is the obvious thing somebody would reach for if
        // `MAX_LASSO_RING` ever needed more headroom.
    }

    #[test]
    fn a_very_long_lasso_is_not_subdivided_into_a_slow_frame() {
        // Both caps, and neither was guarded when it was written — a critic
        // predicted that deleting `MAX_LASSO_SMOOTHING`'s clamp would leave
        // every test green, and it would have.
        //
        // What is at stake is two costs, not tidiness: `ui::selection_outline`
        // draws one line segment per vertex every frame for as long as the
        // selection stands, and `rasterise` is `height × SUB_SCANLINES ×
        // edges` with no active-edge table. Eight times the vertices is eight
        // times both, on a gesture whose ring is already the largest anybody
        // makes.
        let feed = |count: usize, spacing: f32| {
            let mut draft = SelectionDraft::new(SelectionMode::Lasso, Vec2::ZERO);
            // A slow spiral, so the samples are neither collinear (which would
            // change nothing under corner cutting) nor a closed loop the step
            // could cull.
            for k in 1..count {
                let a = 0.02 * k as f32;
                let r = spacing * k as f32 / std::f32::consts::TAU;
                draft.moved(vec2(a.cos() * r, a.sin() * r), 0.0);
            }
            let raw = draft.points.len();
            let mut ring = Vec::new();
            draft.lasso_ring(&mut ring);
            (raw, ring.len())
        };

        // A gesture past the cap: the smoothing gives way rather than the ring
        // growing. Ten thousand samples is a minute of scrubbing, which is a
        // real thing a hand does when cutting round hair.
        let (raw, ring) = feed(10_000, 40.0);
        assert!(raw > MAX_LASSO_RING / 2, "the fixture is too small to bind");
        assert!(
            ring <= MAX_LASSO_RING,
            "a {raw}-sample lasso came back as {ring} vertices, past the \
             {MAX_LASSO_RING} the marquee and the rasteriser are budgeted for"
        );

        // And a gesture the *pass* cap binds on instead: few enough samples
        // that the ring cap leaves plenty of doublings, coarse enough that the
        // spacing asks for more than three. Deleting `MAX_LASSO_SMOOTHING`'s
        // clamp fails this line and nothing else.
        let (raw, ring) = feed(200, 2000.0);
        assert!(
            ring <= 8 * raw,
            "{raw} samples spaced far apart became {ring} vertices, which is \
             more than three doublings"
        );
        assert!(ring >= 4 * raw, "the fixture never reached the pass cap");
    }

    #[test]
    fn a_lasso_with_no_stabiliser_records_the_points_it_was_given() {
        // Zero has to be the exact identity, and "exact" is the word to test:
        // an alpha of one is `s + (a - s)`, which is not `a` wherever the two
        // are far apart in magnitude. These coordinates are a lasso across the
        // largest canvas Umber will make, where a float's step is already a
        // four-hundredth of a pixel — so the filter's own arithmetic is
        // measurably lossy and the branch is what makes the promise true.
        let far = vec2(32768.0, 32768.0);
        let near = vec2(0.1, 0.2);
        let mut draft = SelectionDraft::new(SelectionMode::Lasso, far);
        draft.moved(near, STEP);
        assert_eq!(
            draft.points,
            vec![far, near],
            "an unstabilised lasso moved the sample it was handed"
        );

        // Which is only worth asserting because the filter really would move
        // it. Stated here rather than trusted, because a test of an exactness
        // whose inexact twin happens to agree is a test of nothing.
        let mut smoothed = far;
        smoothed += (near - smoothed) * 1.0;
        assert_ne!(
            smoothed, near,
            "the filter is exact at these coordinates, so the branch is untested"
        );
    }

    #[test]
    fn a_stabilised_lasso_rounds_what_the_hand_did_and_trails_behind_it() {
        // What a stabiliser is, measured against the same pointer path with the
        // rail at zero — the two drafts are fed one list of samples, so the
        // difference between them is the filter and nothing else.
        //
        // A hand turning a right angle, sixty reports along each leg, which is
        // what crossing a hundred document pixels actually produces.
        let corner = [vec2(0.0, 0.0), vec2(100.0, 0.0), vec2(100.0, 100.0)];
        let mut path = vec![corner[0]];
        for leg in corner.windows(2) {
            for k in 1..=60 {
                path.push(leg[0].lerp(leg[1], k as f32 / 60.0));
            }
        }
        let feed = |stabiliser: f32| {
            let mut d = SelectionDraft::new(SelectionMode::Lasso, path[0]).stabilised(stabiliser);
            for p in &path[1..] {
                d.moved(*p, 0.1);
            }
            d
        };

        let loose = feed(0.0);
        let damped = feed(0.8);

        // It rounds the corner, which is the thing somebody turns it on for.
        let nearest = |d: &SelectionDraft| {
            d.points
                .iter()
                .map(|p| p.distance(corner[1]))
                .fold(f32::MAX, f32::min)
        };
        assert!(
            nearest(&loose) < 1.0,
            "the raw line went through the corner"
        );
        // 3.8 px as it stands, against a raw line that passes exactly through.
        // The bound is under the measurement rather than on it, so that a
        // change to the filter's shape has room to be a change rather than a
        // failure — what this is about is the two lines differing by pixels
        // and not by rounding.
        assert!(
            nearest(&damped) > 2.5,
            "the stabilised line still went through the corner, {:.1} px away",
            nearest(&damped)
        );

        // **And it ends short of the pointer, which is not a defect to fix.**
        // An exponential filter following a moving target settles at a constant
        // lag rather than catching up — `(1 - a) / a` reports' worth, which is
        // four here — so a stabilised lasso stops where the hand was a moment
        // ago. That is what `StrokeBuilder` does to a brush stroke too, and for
        // a *closed* outline it costs nothing anybody can see: the ring's last
        // edge runs back to the first point either way. Pulling the filter onto
        // the release would be a jump at exactly the moment the shape is
        // committed.
        let hard = *loose.points.last().expect("a point");
        let soft = *damped.points.last().expect("a point");
        assert_eq!(hard, corner[2], "the unstabilised line ends at the pointer");
        let lag = soft.distance(corner[2]);
        assert!(
            (3.0..12.0).contains(&lag),
            "the stabilised line ended {lag:.1} px behind the pointer, which is \
             not the four reports' lag this filter has"
        );

        // The lag is a property of the target moving, not a permanent offset:
        // a hand that stops is arrived at.
        let mut resting = damped.clone();
        for _ in 0..200 {
            resting.moved(corner[2], 0.0);
        }
        assert!(
            resting.points.last().expect("a point").distance(corner[2]) < 0.01,
            "the filter never reached a pointer that stopped moving"
        );
    }

    // --- combining --------------------------------------------------------

    /// How many pixels of `s` are at least half selected. The booleans are
    /// about *area*, so the tests below count it rather than sampling.
    fn covered(s: &Selection) -> usize {
        s.coverage().iter().filter(|c| **c >= 128).count()
    }

    #[test]
    fn adding_a_disjoint_shape_selects_both_and_grows_the_bounds() {
        // The point of Add: two areas with nothing between them are one
        // selection. The bounding rectangle has to grow to hold both, and the
        // mask has to be re-origined onto it — an off-by-one there puts every
        // selected pixel one across, on a mask that still looks plausible.
        let a = rect(4.0, 4.0, 10.0, 10.0);
        let b = rect(30.0, 40.0, 40.0, 50.0);
        let both = a.union(&b).expect("two areas");

        assert_eq!(
            both.bounds(),
            PixelRect {
                x: 4,
                y: 4,
                width: 36,
                height: 46
            }
        );
        assert_eq!(both.coverage().len(), 36 * 46);
        // Both shapes, in their own places, and nothing joining them.
        assert_eq!(both.coverage_at(5, 5), 255);
        assert_eq!(both.coverage_at(9, 9), 255);
        assert_eq!(both.coverage_at(31, 41), 255);
        assert_eq!(both.coverage_at(39, 49), 255);
        assert_eq!(both.coverage_at(20, 20), 0, "the gap stays out");
        assert_eq!(covered(&both), 36 + 100);
        // Two regions, so two rings — not one ring round the pair.
        assert_eq!(both.rings().len(), 2);
        assert!(both.contains(vec2(6.0, 6.0)));
        assert!(both.contains(vec2(35.0, 45.0)));
        assert!(!both.contains(vec2(20.0, 20.0)));
    }

    #[test]
    fn adding_a_touching_shape_merges_it_into_one_region() {
        // Two rectangles sharing an edge. The area is the sum, and the outline
        // is *one* ring: the seam between them is not a boundary of the union,
        // so tracing must not leave it in.
        let a = rect(10.0, 10.0, 20.0, 20.0);
        let b = rect(20.0, 10.0, 30.0, 20.0);
        let merged = a.union(&b).expect("one region");

        assert_eq!(
            merged.bounds(),
            PixelRect {
                x: 10,
                y: 10,
                width: 20,
                height: 10
            }
        );
        assert_eq!(covered(&merged), 200);
        assert_eq!(merged.coverage_at(19, 15), 255);
        assert_eq!(merged.coverage_at(20, 15), 255, "and across the join");
        assert_eq!(merged.rings().len(), 1, "one region, one outline");
        // A rectangle, so four corners once the straight runs are collapsed.
        assert_eq!(merged.rings()[0].len(), 4);
        assert!(merged.contains(vec2(25.0, 15.0)));
    }

    #[test]
    fn adding_a_shape_that_overlaps_does_not_double_the_overlap() {
        // `max`, not `a + b - ab`: adding a shape to itself is the identity.
        let a = rect(10.0, 10.0, 20.0, 20.0);
        let again = a.union(&a).expect("itself");
        assert_eq!(again.bounds(), a.bounds());
        assert_eq!(again.coverage(), a.coverage());
    }

    #[test]
    fn subtracting_half_a_rectangle_leaves_exactly_the_other_half() {
        // The plain statement of what Subtract is for. Half of a 20×20 box is
        // taken away and the remaining area, the remaining outline and the
        // trimmed bounds all have to agree that it went.
        let whole = rect(10.0, 10.0, 30.0, 30.0);
        let right_half = rect(20.0, 10.0, 30.0, 30.0);
        let left = whole.difference(&right_half).expect("the left half");

        assert_eq!(
            left.bounds(),
            PixelRect {
                x: 10,
                y: 10,
                width: 10,
                height: 20
            },
            "the bounds shrink onto what is left"
        );
        assert_eq!(left.coverage().len(), 200);
        assert_eq!(covered(&left), 200);
        assert_eq!(left.coverage_at(19, 20), 255);
        assert_eq!(left.coverage_at(20, 20), 0);
        assert!(left.contains(vec2(15.0, 20.0)));
        assert!(!left.contains(vec2(25.0, 20.0)));
    }

    #[test]
    fn subtracting_the_middle_leaves_a_hole_the_outline_knows_about() {
        // A hole is the case the winding of a traced ring decides. The inner
        // ring has to come out wound the opposite way to the outer one, or
        // nonzero winding fills the hole straight back in and `contains`
        // disagrees with the mask it was traced from.
        let whole = rect(10.0, 10.0, 40.0, 40.0);
        let middle = rect(20.0, 20.0, 30.0, 30.0);
        let ring = whole.difference(&middle).expect("a frame");

        assert_eq!(ring.bounds(), whole.bounds(), "the outside is unchanged");
        assert_eq!(covered(&ring), 900 - 100);
        assert_eq!(ring.coverage_at(25, 25), 0);
        assert_eq!(ring.coverage_at(15, 25), 255);
        assert_eq!(
            ring.rings().len(),
            2,
            "an outer ring and one round the hole"
        );
        assert!(!ring.contains(vec2(25.0, 25.0)), "the hole is not selected");
        assert!(ring.contains(vec2(15.0, 25.0)));
    }

    #[test]
    fn subtracting_everything_selects_nothing() {
        // Down to nothing is the same answer as never having had one, because
        // every caller treats an empty selection and no selection alike — and
        // a `Selection` with a zero-area mask would be a rectangle the renderer
        // must not be handed.
        let a = rect(10.0, 10.0, 20.0, 20.0);
        assert!(a.difference(&a).is_none());
        let over = rect(0.0, 0.0, 64.0, 64.0);
        assert!(a.difference(&over).is_none());
    }

    #[test]
    fn intersecting_two_overlapping_boxes_keeps_only_the_overlap() {
        // The plain statement of what Intersect is for, and the one boolean
        // whose bounding rectangle is smaller than *both* operands — which is
        // the half that is silent when it is wrong, since outside the rectangle
        // is unselected rather than clamped.
        let a = rect(10.0, 10.0, 30.0, 30.0);
        let b = rect(20.0, 20.0, 40.0, 40.0);
        let both = a.intersection(&b).expect("the overlap");

        assert_eq!(
            both.bounds(),
            PixelRect {
                x: 20,
                y: 20,
                width: 10,
                height: 10
            }
        );
        assert_eq!(both.coverage().len(), 100);
        assert_eq!(covered(&both), 100);
        assert_eq!(both.coverage_at(20, 20), 255);
        assert_eq!(both.coverage_at(29, 29), 255);
        // Everything either one covered alone is gone. Reading these off
        // `coverage_at` is the point: a mask sized to the larger rectangle
        // would have left this selection's own coverage standing here, which is
        // a union wearing intersect's name.
        assert_eq!(both.coverage_at(15, 15), 0);
        assert_eq!(both.coverage_at(35, 35), 0);
        assert_eq!(both.rings().len(), 1);
        assert!(both.contains(vec2(25.0, 25.0)));
        assert!(!both.contains(vec2(15.0, 15.0)));
    }

    #[test]
    fn intersecting_with_itself_is_the_identity_and_with_a_stranger_is_nothing() {
        // `min`, like the union's `max`, is idempotent — so the round trip
        // through the mask, the trim and the trace must give the same mask
        // back. And two shapes that share no pixel intersect to `None` rather
        // than to a zero-area rectangle the renderer must not be handed.
        let a = rect(10.0, 10.0, 20.0, 20.0);
        let again = a.intersection(&a).expect("itself");
        assert_eq!(again.bounds(), a.bounds());
        assert_eq!(again.coverage(), a.coverage());

        let far = rect(40.0, 40.0, 50.0, 50.0);
        assert!(a.intersection(&far).is_none());
        // Touching along one edge is not overlapping: the rectangles meet at
        // x = 20 and share no pixel, because the mask is half-open there.
        let beside = rect(20.0, 10.0, 30.0, 20.0);
        assert!(a.intersection(&beside).is_none());
    }

    #[test]
    fn an_intersection_reads_the_same_from_either_side() {
        // `min` is commutative and the overlap of two rectangles is symmetric,
        // so the two orders have to agree pixel for pixel — antialiased band
        // along the triangle's diagonal included, which is where a difference
        // between the two would actually show up. Worth pinning because the
        // implementation is *not* symmetric: one operand seeds the mask and the
        // other bounds it.
        let a = Selection::polygon(&[vec2(8.0, 8.0), vec2(40.0, 12.0), vec2(14.0, 44.0)], DOC)
            .expect("a triangle");
        let b = rect(16.0, 16.0, 36.0, 36.0);

        let one = a.intersection(&b).expect("an overlap");
        let other = b.intersection(&a).expect("the same overlap");
        assert_eq!(one.bounds(), other.bounds());
        assert_eq!(one.coverage(), other.coverage());
    }

    #[test]
    fn intersecting_with_something_that_swallows_it_changes_nothing() {
        // The case that says the operation is a *bound* rather than a second
        // mask: where the other selection is fully inside, every pixel of this
        // one is already the smaller of the two and the answer is itself.
        let small =
            Selection::polygon(&[vec2(20.0, 20.0), vec2(34.0, 24.0), vec2(24.0, 36.0)], DOC)
                .expect("a triangle");
        let around = rect(10.0, 10.0, 50.0, 50.0);
        let same = small.intersection(&around).expect("itself");
        // Compared over the document rather than as two masks: the result is
        // trimmed to what it covers and the operand's rectangle is the outline's
        // box rounded outwards, so the two can legitimately differ by a row that
        // holds nothing.
        for y in 0..DOC.y {
            for x in 0..DOC.x {
                assert_eq!(
                    same.coverage_at(x, y),
                    small.coverage_at(x, y),
                    "at ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn the_empty_cases_of_a_combination_each_answer_differently() {
        let base = rect(10.0, 10.0, 20.0, 20.0);
        let shape = rect(30.0, 30.0, 40.0, 40.0);

        // A bare click deselects — but only without a modifier. A slip of the
        // hand while adding or subtracting must leave the work alone.
        assert!(Selection::combined(Some(&base), None, SelectionOp::Replace).is_none());
        let kept = Selection::combined(Some(&base), None, SelectionOp::Add).expect("kept");
        assert_eq!(kept.bounds(), base.bounds());
        let kept = Selection::combined(Some(&base), None, SelectionOp::Subtract).expect("kept");
        assert_eq!(kept.bounds(), base.bounds());

        // Adding to nothing is the shape itself, and it comes through
        // untouched — the same rings, not rings traced back out of a mask.
        let fresh = Selection::combined(None, Some(shape.clone()), SelectionOp::Add).expect("new");
        assert_eq!(fresh.rings(), shape.rings());

        // Subtracting from nothing is nothing: no selection means the whole
        // document, and taking a shape out of it would claim more than the
        // gesture did.
        assert!(Selection::combined(None, Some(shape.clone()), SelectionOp::Subtract).is_none());

        // Replace never looks at the base, and never re-traces.
        let replaced =
            Selection::combined(Some(&base), Some(shape.clone()), SelectionOp::Replace).unwrap();
        assert_eq!(replaced.rings(), shape.rings());
    }

    #[test]
    fn a_gesture_carries_the_operation_it_began_with() {
        // Snapshotted at the start: a modifier let go of halfway through a
        // lasso, or tapped between two clicks of a polygon, must not change
        // what the gesture turns out to have meant.
        let draft = SelectionDraft::new(SelectionMode::Rectangle, vec2(10.0, 10.0))
            .combining(SelectionOp::Subtract);
        assert_eq!(draft.op(), SelectionOp::Subtract);
        assert_eq!(
            SelectionDraft::new(SelectionMode::Lasso, vec2(0.0, 0.0)).op(),
            SelectionOp::Replace,
            "an unmodified gesture replaces"
        );
    }

    #[test]
    fn two_areas_touching_only_at_a_corner_keep_their_own_outlines() {
        // The one ambiguous case in the trace: two selected pixels meeting
        // diagonally across two unselected ones. The walk turns as sharply as
        // it can, so they stay two regions — pinching them into one ring would
        // put an outline through pixels that are not selected.
        let a = rect(10.0, 10.0, 20.0, 20.0);
        let b = rect(20.0, 20.0, 30.0, 30.0);
        let both = a.union(&b).expect("two squares corner to corner");
        assert_eq!(covered(&both), 200);
        assert_eq!(both.rings().len(), 2);
        assert!(
            !both.contains(vec2(25.0, 15.0)),
            "the empty quarters stay out"
        );
        assert!(!both.contains(vec2(15.0, 25.0)));
    }

    #[test]
    fn an_outline_is_written_into_the_callers_buffer() {
        // The one thing here that runs per frame. It must not allocate, which
        // is why it takes a buffer rather than returning one — and the buffer
        // has to be cleared, or the outline grows a tail of last frame's.
        let mut buf = vec![Vec2::ZERO; 9];
        let mut draft = SelectionDraft::new(SelectionMode::Rectangle, vec2(10.0, 10.0));
        draft.moved(vec2(30.0, 20.0), STEP);
        draft.outline_into(&mut buf);
        assert_eq!(
            buf,
            vec![
                vec2(10.0, 10.0),
                vec2(30.0, 10.0),
                vec2(30.0, 20.0),
                vec2(10.0, 20.0)
            ]
        );

        let mut poly = SelectionDraft::new(SelectionMode::Polygon, vec2(0.0, 0.0));
        poly.press(vec2(10.0, 0.0), 4.0);
        poly.moved(vec2(10.0, 10.0), STEP);
        poly.outline_into(&mut buf);
        assert_eq!(buf.len(), 3, "the rubber band is part of the outline");
    }

    // --- feather ----------------------------------------------------------

    #[test]
    fn no_feather_is_the_exact_identity() {
        // The same rule the grain's `mix(1.0, tile, strength)` and the dab
        // pass's `use_selection` hold to: a selection nobody softened must cost
        // exactly what it did before feathering existed, down to the bytes and
        // the rectangle they sit in. A blur that "did nothing" by convolving
        // with a one-tap kernel would still round every byte and still
        // reallocate.
        let s = rect(10.0, 10.0, 20.0, 20.0);
        for radius in [0.0, -1.0] {
            let same = s.clone().feathered(radius, DOC).expect("itself");
            assert_eq!(same.bounds(), s.bounds());
            assert_eq!(same.coverage(), s.coverage());
            assert_eq!(same.feather(), 0.0);
            assert_eq!(same.rings(), s.rings());
        }
    }

    #[test]
    fn a_feather_grows_the_rectangle_and_ramps_across_the_edge() {
        // The bounding rectangle is what sizes the texture the dab pass
        // samples, and outside it is decided arithmetically rather than by
        // clamping — so a rectangle that did not grow would chop the falloff
        // off square, which is the feather not happening at all.
        let s = rect(20.0, 20.0, 40.0, 40.0)
            .feathered(4.0, DOC)
            .expect("a soft box");
        let b = s.bounds();
        assert!(b.x <= 16 && b.y <= 16, "the rectangle grew: {b:?}");
        assert!(
            b.x + b.width >= 44 && b.y + b.height >= 44,
            "and grew at the far side too: {b:?}"
        );
        assert_eq!(s.feather(), 4.0);

        // Full in the middle, nothing well outside, and monotone in between.
        assert_eq!(s.coverage_at(30, 30), 255);
        assert_eq!(s.coverage_at(30, 14), 0);
        let ramp: Vec<u8> = (16..24).map(|y| s.coverage_at(30, y)).collect();
        assert!(
            ramp.windows(2).all(|w| w[0] <= w[1]),
            "the falloff has to climb the whole way: {ramp:?}"
        );
        assert!(
            ramp[0] < 40 && *ramp.last().expect("a ramp") > 215,
            "and has to actually get from one end to the other: {ramp:?}"
        );
    }

    #[test]
    fn a_feathered_rectangle_is_still_exact_on_both_axes() {
        // The promise the fill rule makes for an axis-aligned box, carried
        // through the blur. The kernel is separable and the sharp mask of a
        // rectangle is itself the product of two one-dimensional steps, so the
        // softened coverage is the product of two identical ramps: the same
        // figures across the left edge as down the top one, and mirrored
        // exactly about the box's own centre. Any asymmetry here would mean a
        // sub-scanline or a running sum had drifted.
        let s = rect(20.0, 20.0, 40.0, 40.0)
            .feathered(5.0, DOC)
            .expect("a soft box");

        for d in 0..12u32 {
            let across = s.coverage_at(15 + d, 30);
            let down = s.coverage_at(30, 15 + d);
            assert_eq!(across, down, "the two axes disagree {d} in");
            // The far side of the box is the same ramp run backwards: 20 and 40
            // are the two edges, so 15 + d and 44 - d are the same distance out.
            assert_eq!(
                across,
                s.coverage_at(44 - d, 30),
                "the left and right edges are not mirrored, {d} in"
            );
            assert_eq!(across, s.coverage_at(30, 44 - d));
        }
        // And the outline is where the coverage is half — which is the property
        // the whole "keep the rings" decision rests on. The edge is at document
        // x = 20.0, which is the *line between* pixels 19 and 20 rather than
        // either of their centres, so what has to come to 255 is the pair: the
        // kernel is symmetric about that line and the two share the half.
        let outside = u16::from(s.coverage_at(19, 30));
        let inside = u16::from(s.coverage_at(20, 30));
        assert!(
            outside.abs_diff(255 - inside) <= 1,
            "the falloff is not centred on the outline: {outside} outside and \
             {inside} inside the edge at x = 20"
        );
    }

    #[test]
    fn a_feather_leaves_the_outline_exactly_where_it_was() {
        // The decision the module docs defend: the blur is symmetric, so its
        // 50% contour *is* the sharp edge, and the rings stay the exact
        // geometry rather than being traced back out of a softened mask. The
        // marquee, `contains`, and everything a transform commit or a canvas
        // flip re-rasterises all read those rings.
        let sharp =
            Selection::polygon(&[vec2(10.0, 10.0), vec2(40.0, 14.0), vec2(16.0, 44.0)], DOC)
                .expect("a triangle");
        let soft = sharp.clone().feathered(6.0, DOC).expect("a soft triangle");
        assert_eq!(soft.rings(), sharp.rings());
        assert!(soft.contains(vec2(20.0, 20.0)));
        assert!(!soft.contains(vec2(38.0, 40.0)));
    }

    #[test]
    fn a_feather_fades_at_the_edge_of_the_canvas_rather_than_running_off_it() {
        // Nothing downstream may be handed a rectangle that leaves the texture,
        // and outside the canvas is not selected — so a selection against the
        // edge softens into it. That is what Photoshop and GIMP do; treating
        // the canvas edge as more of the same would put a hard edge nobody drew
        // round a feathered Select All.
        let s = rect(0.0, 0.0, 20.0, 20.0)
            .feathered(4.0, DOC)
            .expect("a soft box in the corner");
        let b = s.bounds();
        assert_eq!((b.x, b.y), (0, 0), "clamped to the canvas");
        assert!(b.x + b.width <= DOC.x && b.y + b.height <= DOC.y);
        assert!(
            s.coverage_at(0, 0) < 200,
            "the corner against the canvas edge fades like every other edge"
        );
        assert_eq!(s.coverage_at(10, 10), 255);
    }

    #[test]
    fn a_feather_survives_a_canvas_flip() {
        // `flipped` rebuilds the mask by rasterising the mirrored rings, which
        // is sharp — so without re-applying the radius a flip would silently
        // harden every soft edge in the picture, and undoing the flip would not
        // put it back. Compared on the *mask*, because that is what clips a
        // stroke.
        let s = rect(10.0, 12.0, 30.0, 24.0)
            .feathered(5.0, DOC)
            .expect("a soft box");
        let there = s.flipped(FlipAxis::Horizontal, DOC).expect("a mirror");
        assert_eq!(there.feather(), 5.0);

        let back = there
            .flipped(FlipAxis::Horizontal, DOC)
            .expect("a mirror back");
        assert_eq!(back.bounds(), s.bounds());
        assert_eq!(back.coverage(), s.coverage());
    }

    #[test]
    fn a_flip_never_deletes_a_selection_it_cannot_soften() {
        // A small region carrying a wide feather is reachable, because a
        // boolean traces its rings at the 50% contour and records the *larger*
        // of the two radii. Re-rasterising those rings sharp and softening them
        // by that radius can round every pixel to nothing — and a flip that
        // silently threw the selection away would not even be undoable, since
        // undoing a flip is another flip.
        let small = rect(28.0, 28.0, 32.0, 32.0);
        // Softening it outright *is* nothing, which is the state the flip has
        // to survive rather than pass on.
        assert!(
            small
                .clone()
                .feathered(Selection::MAX_FEATHER, DOC)
                .is_none()
        );

        // The state a boolean can leave: these rings, that radius. Built by
        // hand because reaching it through two feathered lassos would be a
        // fixture nobody could check by eye, and the shape of the state is the
        // whole point.
        let awkward = Selection {
            feather: Selection::MAX_FEATHER,
            ..small
        };
        let flipped = awkward
            .flipped(FlipAxis::Horizontal, DOC)
            .expect("the selection must survive a flip");
        assert!(
            flipped.coverage().iter().any(|c| *c > 0),
            "the flip left a selection with no coverage"
        );
        // And it is the hard mirror, in the mirrored place — 64 - 32 .. 64 - 28.
        assert_eq!(flipped.bounds().x, 32);
        assert_eq!(flipped.coverage_at(32, 30), 255);
    }

    #[test]
    fn a_boolean_carries_the_softer_of_the_two_edges() {
        // One number cannot say that half an outline is soft, and of the two
        // answers available only the larger cannot harden an edge that was.
        let hard = rect(10.0, 10.0, 30.0, 30.0);
        let soft = rect(20.0, 20.0, 40.0, 40.0)
            .feathered(3.0, DOC)
            .expect("a soft box");

        assert_eq!(hard.union(&soft).expect("a union").feather(), 3.0);
        assert_eq!(soft.union(&hard).expect("a union").feather(), 3.0);
        assert_eq!(hard.difference(&soft).expect("a cut").feather(), 3.0);
        assert_eq!(hard.intersection(&soft).expect("an overlap").feather(), 3.0);
    }

    #[test]
    fn a_gesture_carries_the_feather_it_began_with() {
        // Snapshotted at the start, exactly as the operation is: a polygon
        // spans several clicks, and a rail dragged between two of them must not
        // change what the gesture already under way turns out to have meant.
        let mut draft =
            SelectionDraft::new(SelectionMode::Rectangle, vec2(10.0, 10.0)).feathered(4.0);
        assert_eq!(draft.feather(), 4.0);
        draft.moved(vec2(30.0, 30.0), STEP);
        let s = draft.finish(DOC).expect("a soft box");
        assert_eq!(s.feather(), 4.0);
        assert!(s.coverage_at(9, 20) > 0, "the edge softened outwards");

        assert_eq!(
            SelectionDraft::new(SelectionMode::Lasso, vec2(0.0, 0.0)).feather(),
            0.0,
            "a gesture nobody softened is hard"
        );
    }

    #[test]
    fn a_feather_is_bounded_however_it_is_asked_for() {
        // The radius reaches this module from a control somebody can type into,
        // and it decides how far the bounding rectangle grows — so an absurd
        // one is clamped here rather than trusted to the interface.
        let big = UVec2::splat(1024);
        let s =
            Selection::rectangle(vec2(200.0, 200.0), vec2(800.0, 800.0), big).expect("a rectangle");
        let wild = s
            .feathered(Selection::MAX_FEATHER * 10.0, big)
            .expect("still a selection");
        assert_eq!(wild.feather(), Selection::MAX_FEATHER);
        let b = wild.bounds();
        assert!(
            b.x + b.width <= big.x && b.y + b.height <= big.y,
            "the grown rectangle left the canvas: {b:?}"
        );
    }

    #[test]
    fn a_feather_wide_enough_to_dissolve_a_shape_selects_nothing() {
        // The tent normalises, so a radius far larger than the shape spreads
        // its coverage below a byte everywhere — and a mask of nothing is
        // nothing selected, the same answer as a lasso that enclosed no pixel
        // and as a subtraction that took everything. The alternative would be a
        // `Selection` with an all-zero mask, which is an outline standing over
        // a region that clips every stroke to nothing.
        let s = rect(20.0, 20.0, 24.0, 24.0);
        assert!(s.feathered(Selection::MAX_FEATHER, DOC).is_none());
    }

    /// The marquee has to travel with a flipped canvas, or it goes on clipping
    /// a region that no longer holds what the artist marked out.
    ///
    /// Pinned on a rectangle because that is the one shape the rasteriser is
    /// exact on both axes for, so this is equality rather than a tolerance: the
    /// mirrored selection covers exactly the pixels the mirrored picture put
    /// under it.
    #[test]
    fn a_flipped_selection_covers_the_pixels_the_flip_moved_under_it() {
        let s = rect(10.0, 12.0, 20.0, 18.0);
        let flipped = s.flipped(FlipAxis::Horizontal, DOC).expect("a mirror");
        assert_eq!(
            flipped.bounds(),
            PixelRect {
                x: 44,
                y: 12,
                width: 10,
                height: 6
            },
            "64 - 20 .. 64 - 10"
        );
        assert_eq!(flipped.coverage_at(44, 12), 255);
        assert_eq!(flipped.coverage_at(43, 12), 0);
        assert_eq!(flipped.coverage_at(53, 17), 255);

        let vertical = s.flipped(FlipAxis::Vertical, DOC).expect("a mirror");
        assert_eq!(
            vertical.bounds(),
            PixelRect {
                x: 10,
                y: 46,
                width: 10,
                height: 6
            }
        );

        // And it is its own inverse, which is what lets undoing a flip be
        // another flip on this side as well as on the GPU's. Compared on the
        // mask rather than on the rings, because that is what actually clips a
        // stroke.
        let back = flipped
            .flipped(FlipAxis::Horizontal, DOC)
            .expect("a mirror back");
        assert_eq!(back.bounds(), s.bounds());
        assert_eq!(back.coverage(), s.coverage());
    }
}
