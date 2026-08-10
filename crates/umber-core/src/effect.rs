//! Layer effects: the parameters, the order they composite in, and the cap.
//!
//! Non-destructive marks derived from a layer's own alpha and composited around
//! it — a stroke, a drop shadow. The layer's pixels are not touched, the
//! parameters stay editable, and the file carries the parameters rather than the
//! result. `docs/layer-effects.md` is the design and has the whole argument.
//!
//! **This is the model and nothing here reaches a shader**, which is what makes
//! the parameter set, the ordering, the refusals and the serialised spelling
//! settled and testable without a device. It used to say the whole feature was
//! inert, and that was true of stage 0 and is not true now: `umber-render`'s
//! `bake_effects` reads [`Effect`] and produces a `LayerDraw` per enabled one, so
//! everything below is live. The interface that would let somebody *make* one is
//! stage 2 and is not built, so today an effect arrives from a file.
//!
//! §12 describes stage 0 as emitting `LayerDraw`s that point at slices holding
//! nothing, shipped disabled behind an empty effect set. It never did: a document
//! with no effects produced exactly the draw list it produced before, entry for
//! entry, and still does — the same guarantee with one fewer moving part, since
//! an empty set is a state a control could leave where a module the composite has
//! never heard of cannot be.
//!
//! # `Outline` in code, "Stroke" in the interface
//!
//! **`Stroke` is already taken four times over**: [`crate::stroke`] is the brush
//! stroke, `StrokeBuilder` generates its dabs, `StrokeStyle` is what the preview
//! and the commit are both handed, and `stroke_tex`, `stroke_color`,
//! `stroke_blend` and `stroke_on_mask` are fields of the composite's uniform. A
//! layer effect called `Stroke` would collide with every one of them, in the
//! same files.
//!
//! So the effect is [`EffectKind::Outline`] here and **no type, variant, field
//! or function under `umber-core` or `umber-render` may spell a *layer effect*
//! "Stroke"** — the four above keep the name for the brush stroke, which is
//! exactly the collision this avoids rather than an exception to it. The
//! interface must say Stroke, because that is the name painters know it by from
//! Photoshop and from Krita's layer styles, and the one place that word appears
//! is [`EffectKind::label`] — a string rather than an identifier, which is
//! exactly what makes the rule above something you can hold to. Same shape as
//! the `theme::text` / `umber_core::text` collision: import the item, never the
//! module, written down before the collision rather than after.
//!
//! # An effect is `Copy`, and that settles the design's open question
//!
//! [`Effect`] is one flat struct carrying every parameter, the way [`crate::
//! Brush`] is, rather than a per-kind variant carrying only its own. Two
//! consequences and both were the point:
//!
//! * **It is `Copy`.** `docs/layer-effects.md` §13 left open whether effects
//!   belong on [`crate::Layer`] given that a `Layer` is `Clone` and cheap, and
//!   whether a parameter set per kind would cost [`crate::history::EditBody`]'s
//!   structural entries something. It does not: a layer holds at most one effect
//!   per kind, so at most `EffectKind::ALL.len()` of these, each a few dozen
//!   bytes of plain data with no allocation in it. Cloning a layer stays a
//!   `String`, a `Vec` of two `Copy` structs and some flags. **§13's bullet can
//!   be struck.**
//! * A field a kind does not read is carried and ignored — `distance` on an
//!   outline, `position` on a drop shadow. That is the cost, it is a handful of
//!   bytes, and it buys that the interface can dial a shadow's angle, switch the
//!   row to an outline and switch back to find the angle where it was left.
//!
//! # What an effect is a function of
//!
//! **One layer's coverage — its alpha, after its mask — and nothing else.**
//! Photoshop's rule for a clipped group is deliberately not copied: there, a
//! base layer's effects apply across every layer clipped to it, which means
//! baking from a composite of several layers rather than from one slice. That is
//! the group-compositing problem (`docs/group-compositing.md`) and it is out of
//! scope. Said here so that somebody comparing against Photoshop finds the
//! answer rather than a bug.
//!
//! The other direction falls out and needs nothing: a *clipped* layer with
//! effects of its own gets its effects clipped with it, because they are draws
//! sitting between the same neighbours carrying the same flag.

use serde::{Deserialize, Serialize};

use crate::color::Color;
use crate::layer::BlendMode;

/// How many **enabled** effects one document may hold.
///
/// `docs/layer-effects.md` §6.3's arithmetic, and **the device's guarantee is
/// what it starts from** rather than anything Umber chose:
///
/// ```text
/// DEVICE_SLICES     = 256                            // max_texture_array_layers
/// MAX_EFFECT_SLICES = DEVICE_SLICES − (MAX × 2 + 1)  // = 127
/// MAX_DRAWS         = MAX + MAX_EFFECT_SLICES        // = 191
/// ```
///
/// `downlevel_defaults()` inherits `max_texture_array_layers` from `defaults()`
/// and `using_resolution` raises only the three texture *dimension* limits, so
/// 256 is the ceiling and a 257th slice is a `create_texture` validation error —
/// which `crash::device_error` makes fatal. An effect draw reads an effect slice
/// one for one, which is why the draw budget can never exceed the slice budget.
/// `DEVICE_SLICES` is written out rather than spelled `LayerStack::MAX_SLOTS`,
/// which now also holds 256. Two literals for one quantity is the cost, and it
/// is paid deliberately: this derivation is about what the *device* guarantees,
/// where `MAX_SLOTS` is about how many slices Umber will hand out, and the two
/// agreeing today is a fact rather than a definition. They are tied by
/// `the_slice_ceiling_agrees_with_umber_core` in `umber-render`, which is the
/// only crate that can see both.
///
/// **Which is why the derivation is asserted here rather than promised for the
/// merge.** [`BUDGET_DERIVATION`] is a `const` assertion over
/// `LayerStack::MAX` and the device figure, both of which this tree holds, so
/// raising `MAX` without re-deriving the budget fails the *build* — and it
/// names nothing the capacity branch is touching, so it merges either way. What
/// is genuinely not statable here is `MAX_DRAWS` against `canvas.rs` and
/// `composite.wgsl`, and the third statement of it that
/// `docs/layer-effects.md` §6.3 wants — `DEVICE_SLICES` against
/// `downlevel_defaults().max_texture_array_layers`, which `umber-core` cannot
/// see because it may not depend on wgpu. **That one lives in `canvas.rs`**,
/// beside that crate's own `MAX_SLOTS`, as a `const` assertion against
/// `downlevel_defaults()` — so the device figure is checked where wgpu is
/// visible and reaches this crate only through the test named above. A truncated
/// draw list is the outcome all of them exist to keep unreachable, because a
/// list cut off mid-group leaves an accumulator open.
///
/// A *disabled* effect costs nothing: it produces no draw, so it is not counted.
///
/// **The cap is live in an ordinary document.** Two kinds, one of each per
/// layer, 64 layers: a full stack asks for 128 and the 128th is refused. It was
/// 128 in the first draft of this module — from a `MAX_DRAWS` of 192 that the
/// device could not have supplied — and at that figure nothing could reach it,
/// because `64 × 2` was exactly the cap. The corrected number is one lower and
/// that is the whole difference between a guard nothing exercises and a refusal
/// somebody will meet.
///
/// **Exceeding it by *undo* is legal and is handled downstream.** The cap
/// governs *adding* an effect; overflow is the draw path's, on §6.1's rule for
/// the byte budget — the effect stays enabled and the document is said to be
/// over its budget, rather than an effect silently vanishing.
/// `LayerStack::restore_shape` therefore does **not** consult this and must not:
/// an undo puts a deleted layer back with the effects it left holding, and an
/// undo cannot be refused. [`LayerStack::set_effect`] is the gate, and it is the
/// only thing that writes an effect.
pub const MAX_ENABLED: usize = 127;

/// The device's guaranteed `max_texture_array_layers`, which is where
/// [`MAX_ENABLED`] comes from. See that constant.
pub const DEVICE_SLICES: usize = 256;

/// [`MAX_ENABLED`] is what the derivation says it is, checked at **compile
/// time**.
///
/// A literal budget with a comment naming its derivation is a budget that goes
/// wrong silently the first time [`crate::LayerStack::MAX`] moves: raise it to
/// 100 and the correct figure is 55, while 127 would ask the device for 72
/// slices past its guarantee — the fatal `create_texture` validation error the
/// whole derivation exists to avoid. Everything it reads is in this crate,
/// which is what makes it a compile error rather than a test.
const BUDGET_DERIVATION: () = assert!(
    MAX_ENABLED == DEVICE_SLICES - (crate::layer::LayerStack::MAX * 2 + 1),
    "the effect budget no longer follows from the device's slice guarantee"
);
const _: () = BUDGET_DERIVATION;

/// Where the layer's own draw sits in `docs/layer-effects.md` §4's numbering.
///
/// An effect whose [`Effect::rank`] is below this composites **under** the
/// layer, one above it **over**.
pub const LAYER_RANK: u8 = 4;

/// Is a document holding `enabled` enabled effects within its budget?
///
/// A free function so that the boundary is a test rather than a sentence, for
/// the reason `widgets::track_value` is one.
pub fn within_budget(enabled: usize) -> bool {
    enabled <= MAX_ENABLED
}

/// Which effect this is.
///
/// Two, deliberately. `docs/layer-effects.md` §12: the second kind is what
/// proves that a stroke and a drop shadow really are one pipeline with different
/// parameters, and the fourth proves nothing. Outer glow, inner shadow, inner
/// glow and colour overlay arrive with their bakes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EffectKind {
    DropShadow,
    /// **"Stroke" in the interface.** See the module docs for why the code may
    /// not say so.
    Outline,
}

impl EffectKind {
    /// Every kind.
    ///
    /// Hand-written, and guarded by an exhaustive `match` in a test rather than
    /// by anything that iterates this array — a test walking `ALL` can only ever
    /// check what is in it. See `all_lists_every_effect_kind`.
    pub const ALL: [EffectKind; 2] = [Self::DropShadow, Self::Outline];

    /// What the interface calls it.
    ///
    /// The **one** place the word "Stroke" appears for a layer effect, and it is
    /// a string rather than an identifier on purpose. See the module docs.
    pub fn label(self) -> &'static str {
        match self {
            Self::DropShadow => "Drop shadow",
            Self::Outline => "Stroke",
        }
    }
}

/// Where an outline sits against the edge it traces.
///
/// British spelling, like everything else the interface says — and this one
/// reaches a file, so `Centre` is a **format** and not merely a name. See
/// `the_serialised_names_of_an_outline_position_are_these_exact_strings`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OutlinePosition {
    /// Entirely outside the coverage. Composites under the layer.
    #[default]
    Outside,
    /// Straddling the edge. Composites under the layer, because the half of it
    /// that shows is the half outside.
    Centre,
    /// Entirely inside the coverage. Composites over the layer, confined to its
    /// alpha — see [`Effect::is_inner`].
    Inside,
}

impl OutlinePosition {
    /// Every position. Guarded as [`EffectKind::ALL`] is.
    pub const ALL: [OutlinePosition; 3] = [Self::Outside, Self::Centre, Self::Inside];

    pub fn label(self) -> &'static str {
        match self {
            Self::Outside => "Outside",
            Self::Centre => "Centre",
            Self::Inside => "Inside",
        }
    }
}

/// One effect on one layer.
///
/// Distances are in **document** pixels, like a brush's, so an effect looks the
/// same at every zoom. Angles are in degrees.
///
/// **The serde defaults are per field and `kind` deliberately has none**, which
/// is where this differs from [`crate::Brush`]'s container `#[serde(default)]`
/// and the difference is not stylistic. A `Brush` has one kind, so one `Default`
/// describes it; an `Effect` is a flat struct over two, and no single default
/// can be right for both. Filling an absent field from a whole-struct default
/// meant an *outline* written before a parameter existed loading with the drop
/// shadow's value for it — a blurred, Multiply outline the artist never set,
/// silently, on the day the parameter was added. So each field defaults to its
/// own **neutral**: nothing grown, nothing softened, nothing displaced, Normal,
/// opaque, black. A file that omits `kind` is *refused* rather than read as an
/// arbitrary one.
///
/// `enabled` is the one that is a **policy rather than a neutral**, and it is
/// worth naming because it is also the one default that costs the budget: an
/// effect written into a file is one somebody made, so it defaults to *on*. The
/// neutral reading would be off, and it would make an effect that predates the
/// field silently stop drawing.
///
/// What the defaults are for is unchanged and is the reason `Brush` has them: a
/// file written before a parameter existed still loads. The direction that
/// matters is a newer build reading an older effect — an older build never sees
/// one at all, because effects raise `umber-version` and it refuses the whole
/// document.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Effect {
    pub kind: EffectKind,
    /// Off means no draw and no bake, and therefore nothing charged against
    /// [`MAX_ENABLED`]. The parameters are kept, so switching it back on gives
    /// back the effect that was dialled rather than a fresh one.
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    /// Written down by [`linear_rgba`], which is the one statement of an effect
    /// colour's serialised form.
    #[serde(default = "black", with = "linear_rgba")]
    pub color: Color,
    /// `0.0..=1.0`, applied to the effect's own draw.
    #[serde(default = "full_opacity")]
    pub opacity: f32,
    /// How the effect's draw combines with what is under it.
    ///
    /// **With the backdrop, not with its own layer** — that is the whole point
    /// of an effect being a draw of its own rather than pixels baked into the
    /// layer's slice, and it is what makes a drop shadow at Multiply do what
    /// Photoshop's default does.
    #[serde(default)]
    pub blend: BlendMode,
    /// How far the shape is grown before it is softened, in document pixels.
    ///
    /// The outline's width; Photoshop's *Spread* on a shadow.
    #[serde(default)]
    pub spread: f32,
    /// Blur radius, in document pixels. Photoshop's *Size*.
    ///
    /// **Zero is the exact identity** — the rule the selection's feather and the
    /// brush's grain both keep, and the one the bake has to hold to.
    #[serde(default)]
    pub softness: f32,
    /// Where the light is, in degrees anticlockwise from the right.
    ///
    /// Photoshop's and Krita's convention, and the one a dial in the interface
    /// will show. The shadow falls *away* from it — see [`Effect::offset`],
    /// which is the only place that is worked out.
    #[serde(default)]
    pub angle: f32,
    /// How far the shadow is displaced, in document pixels.
    #[serde(default)]
    pub distance: f32,
    /// Read by [`EffectKind::Outline`] alone, and carried by every effect for
    /// the reason the module docs give.
    #[serde(default)]
    pub position: OutlinePosition,
}

// The three neutral defaults whose types cannot supply their own. `f32`,
// `BlendMode` and `OutlinePosition` already default to the neutral value — zero,
// Normal, Outside — and take a bare `#[serde(default)]`. **There is deliberately
// no `impl Default for Effect`**: see the struct's docs. An effect with no kind
// is not a thing, and a whole-struct default is what made an outline load
// wearing a drop shadow's parameters.

fn enabled_by_default() -> bool {
    true
}

fn full_opacity() -> f32 {
    1.0
}

fn black() -> Color {
    Color::BLACK
}

impl Effect {
    /// A drop shadow at the settings most applications open one at.
    ///
    /// Multiply rather than Normal, because a shadow that replaces the backdrop
    /// instead of darkening it is not a shadow — and because it is the case
    /// that proves the effect is its own draw.
    pub fn drop_shadow() -> Self {
        Self {
            kind: EffectKind::DropShadow,
            enabled: true,
            color: Color::BLACK,
            opacity: 0.75,
            blend: BlendMode::Multiply,
            spread: 0.0,
            softness: 5.0,
            angle: 120.0,
            distance: 5.0,
            position: OutlinePosition::default(),
        }
    }

    /// An outline — "Stroke" in the interface.
    pub fn outline() -> Self {
        Self {
            kind: EffectKind::Outline,
            enabled: true,
            color: Color::BLACK,
            opacity: 1.0,
            blend: BlendMode::Normal,
            spread: 3.0,
            softness: 0.0,
            angle: 0.0,
            distance: 0.0,
            position: OutlinePosition::Outside,
        }
    }

    /// A fresh effect of `kind`.
    ///
    /// An exhaustive `match`, so a kind added without a starting point for it
    /// fails the build rather than shipping a control that adds an effect with
    /// every parameter at zero.
    pub fn of(kind: EffectKind) -> Self {
        match kind {
            EffectKind::DropShadow => Self::drop_shadow(),
            EffectKind::Outline => Self::outline(),
        }
    }

    /// Where this effect's draw sits in the layer's own run, bottom to top,
    /// numbered as `docs/layer-effects.md` §4 numbers them:
    ///
    /// 1. drop shadow
    /// 2. outer glow
    /// 3. outline, outside or centred
    /// 4. **the layer** ([`LAYER_RANK`])
    /// 5. outline, inside
    /// 6. inner shadow
    /// 7. inner glow
    /// 8. colour overlay
    ///
    /// The gaps are the four kinds that do not exist yet, kept so that adding
    /// one is a number rather than a renumbering of everything around it.
    ///
    /// An exhaustive `match`, twice over: a kind added, or a position added,
    /// fails the build here rather than sorting to an arbitrary place.
    pub fn rank(self) -> u8 {
        match self.kind {
            EffectKind::DropShadow => 1,
            EffectKind::Outline => match self.position {
                OutlinePosition::Outside | OutlinePosition::Centre => 3,
                OutlinePosition::Inside => 5,
            },
        }
    }

    /// Does this composite **under** the layer?
    ///
    /// Position in the draw list and nothing else. It used to say the confinement
    /// followed from it — that an outer effect is multiplied by `1 − coverage` at
    /// bake time — and that is `docs/layer-effects.md` §3.3's rule, which has been
    /// **reversed**: the knockout is the *drop shadow's*, not a property of a side
    /// of the stack. Photoshop's control is called "Layer knocks out drop shadow"
    /// and is named for the shadow because it is the shadow's; generalising it made
    /// a centred outline undrawable, since a stroke sits *on* the edge and removing
    /// it wherever the layer covers deletes the half somebody asked for.
    ///
    /// So an Outside outline is confined in the *grow* (which is what "outside the
    /// edge" means, and where it belongs so a soft stroke is soft on both sides), a
    /// Centre outline is not confined at all, and a drop shadow keeps the knockout
    /// at resolve time — after its blur and its displacement, because what it must
    /// not cover is where the layer is now. `umber-render`'s `EffectShape` is where
    /// that lives.
    pub fn is_outer(self) -> bool {
        self.rank() < LAYER_RANK
    }

    /// Does this composite **over** the layer, confined to its alpha?
    ///
    /// An inner effect, and the confinement is `LayerDraw::clipped` — which
    /// already means "bounded by the alpha of the nearest unclipped layer
    /// below", and an inner effect drawn immediately above its own layer reads
    /// exactly that. **No new mechanism at all.**
    ///
    /// The asymmetry with [`Effect::is_outer`] — outer effects bake their
    /// confinement, inner effects use the clip flag — is the kind of thing that
    /// gets forgotten and reintroduced as a uniform. It is written here so that
    /// it is not.
    pub fn is_inner(self) -> bool {
        self.rank() > LAYER_RANK
    }

    /// How far the effect is displaced, in document pixels, **y-down** like
    /// every other document coordinate in Umber.
    ///
    /// The one place the angle convention is worked out, so the bake cannot
    /// disagree with the interface about which way a shadow falls. [`Effect::
    /// angle`] is where the light is; the shadow goes the other way, which
    /// negates x — and does *not* negate y, because y already points down. At
    /// the 120° default that is down and to the right, which is a light from the
    /// upper left.
    ///
    /// A distance of zero is `(0, 0)` exactly.
    pub fn offset(self) -> (f32, f32) {
        let (sin, cos) = self.angle.to_radians().sin_cos();
        (-self.distance * cos, self.distance * sin)
    }

    /// This effect as it should be on a canvas mirrored about `axis`.
    ///
    /// **A flip mirrors every pixel in the picture and it has to mirror the
    /// lighting with them.** Nothing else on an [`Effect`] is geometry — a spread,
    /// a softness and a distance are lengths and a mirror preserves lengths — so
    /// the whole of it is the angle, and there is no way for this to fail and no
    /// canvas size to pass in. That is what makes it a plain `Self` where
    /// [`crate::textobj::Placement::flipped`] is an `Option`: a text record
    /// carries a *rectangle*, which can be outside a canvas it is asked to mirror
    /// inside, and an effect carries a direction, which cannot.
    ///
    /// The derivation, once, because getting it backwards is a whole document's
    /// shadows disagreeing with its forms and nothing in the code would say so.
    /// [`Effect::offset`] is `(-d·cos a, d·sin a)`. A mirror about the **x** axis
    /// negates the displacement's `x`, so `cos a' = -cos a` and `sin a' = sin a`,
    /// which is `a' = 180 - a`; a mirror about **y** negates its `y`, giving
    /// `cos a' = cos a` and `sin a' = -sin a`, which is `a' = -a`.
    ///
    /// **Exactness is the requirement rather than a nicety, for the reason
    /// `flip.wgsl` is an exact texel permutation: undoing a flip *is* another
    /// flip**, so anything lost here has no other route back. The vertical case
    /// meets it — a negation is exact for every `f32` there is. **The horizontal
    /// case does not**, and that was measured rather than assumed: `180 - a` is
    /// exact only for `a` in `[90, 360]`, so `180.0 - (180.0 - 0.1)` is
    /// `0.100006`. The error is at most half an ulp of 180, does not compound, and
    /// comes to a hundredth of a pixel over the longest displacement a canvas can
    /// hold. `flipping_a_canvas_twice_restores_an_effects_angle` states the bound
    /// and says why the exact alternative — negating the angle *and* the distance,
    /// both exact — was rejected.
    ///
    /// Nothing about a *selection*'s flip applies: that one re-rasterises rings
    /// and keeps a hard mirror so a feather cannot dissolve the rebuilt shape.
    /// An effect is derived from the layer's own coverage every frame, so the
    /// mirrored pixels are the mirrored input and there is nothing to rebuild.
    pub fn flipped(self, axis: crate::geom::FlipAxis) -> Self {
        let angle = match axis {
            crate::geom::FlipAxis::Horizontal => 180.0 - self.angle,
            crate::geom::FlipAxis::Vertical => -self.angle,
        };
        Self { angle, ..self }
    }
}

/// How an [`Effect`]'s colour is written down: the four **linear** components,
/// in the order [`Color::to_array`] gives them.
///
/// [`Color`] deliberately derives no serde and this module is why it still does
/// not. It is the engine's linear RGBA and nothing in Umber had ever needed to
/// write one to a file; a derive on it would make its four field names a format
/// for every future struct that happens to hold a colour, which is the blast
/// radius CLAUDE.md's "a derived serde spelling that reaches a file is a format,
/// not a name" warns about, granted in advance to code nobody has written yet.
/// One `#[serde(with)]` beside the one field that needs it says the same thing
/// and says it where it can be read.
///
/// **Linear `f32` rather than sRGB bytes, unlike [`crate::Swatch`]**, and the
/// difference is the same one that made a swatch eight-bit in the first place. A
/// swatch is *stored and shared*, so holding it in the form it is written in is
/// what makes its round trip exact. An effect's colour is *painted with*: it
/// comes off the picker as a linear value and goes to the bake as one, so
/// quantising it here would be a level lost every time a document was saved and
/// reopened, on a field nothing else quantises.
mod linear_rgba {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use crate::color::Color;

    pub fn serialize<S: Serializer>(color: &Color, s: S) -> Result<S::Ok, S::Error> {
        color.to_array().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Color, D::Error> {
        let [r, g, b, a] = <[f32; 4]>::deserialize(d)?;
        Ok(Color::new(r, g, b, a))
    }
}

/// Put `effects` into composite order, bottom to top.
///
/// The one statement of `docs/layer-effects.md` §4's ordering, kept out of
/// [`crate::layer`] so that it is a function with no stack, no renderer and no
/// draw list anywhere near it. A stable sort on [`Effect::rank`], which is total
/// over every subset because a rank is a function of the effect alone.
///
/// `pub(crate)` rather than `pub`, and the visibility is the enforcement of the
/// sentence rather than a decoration on it: [`crate::Layer`] holds its effects
/// in this order at all times, so nothing outside can need it, and the wider
/// visibility would grant something for nothing. What a consumer reads instead
/// is `Layer::effects_below` and `Layer::effects_above`, which are already the
/// split a draw list wants.
pub(crate) fn sort_into_composite_order(effects: &mut [Effect]) {
    effects.sort_by_key(|e| e.rank());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **These strings are a file format, and what this catches is a rename.**
    ///
    /// An effect's parameters are serialised into the document (`umber/effects/`
    /// — `docs/layer-effects.md` §8.1), so the derived serde spelling of this
    /// enum is what reaches somebody's disk. A rename with no intent behind it
    /// would leave every saved effect unreadable, silently, months later; a
    /// `#[serde(rename = "…")]` would do the same while leaving the `Debug`
    /// spelling untouched, which is why the assertion is against what serde
    /// actually writes rather than against `format!("{:?}")`.
    ///
    /// A kind *added* fails this too, and there appending the name is the right
    /// fix. A kind *renamed* is the case this exists for, and the literal is not
    /// the thing to edit first.
    #[test]
    fn the_serialised_names_of_an_effect_kind_are_these_exact_strings() {
        let spelled: Vec<String> = EffectKind::ALL
            .into_iter()
            .map(|k| ron::to_string(&k).unwrap())
            .collect();
        assert_eq!(spelled, ["DropShadow", "Outline"]);
    }

    /// The twin of the above, and the one with a trap in it: `Centre` is the
    /// British spelling and Photoshop's is `Center`. Somebody "correcting" it
    /// changes what is written to disk.
    #[test]
    fn the_serialised_names_of_an_outline_position_are_these_exact_strings() {
        let spelled: Vec<String> = OutlinePosition::ALL
            .into_iter()
            .map(|p| ron::to_string(&p).unwrap())
            .collect();
        assert_eq!(spelled, ["Outside", "Centre", "Inside"]);
    }

    /// [`EffectKind::ALL`] is hand-written, and a kind missing from it is a kind
    /// the interface cannot offer and the guard above cannot see.
    ///
    /// The guard is the exhaustive `match`, which fails the **build** when a
    /// kind is added. That has to be a compile error rather than an assertion,
    /// because a test that iterates `ALL` can only check the entries that are in
    /// it and so agrees with itself however short the array is.
    ///
    /// The arms index `ALL`, so an arm added the obvious way for a third kind —
    /// `EffectKind::ALL[2]` — is an out-of-bounds index into a fixed-size array
    /// and fails the build a second time when `ALL` was not extended. **An arm
    /// that does not index its own position still slips through**, which is the
    /// hole measured at `history::tests::listed_in_all` and is named here rather
    /// than claimed away.
    #[test]
    fn all_lists_every_effect_kind() {
        const fn listed_in_all(kind: EffectKind) -> EffectKind {
            match kind {
                EffectKind::DropShadow => EffectKind::ALL[0],
                EffectKind::Outline => EffectKind::ALL[1],
            }
        }

        for kind in EffectKind::ALL {
            assert_eq!(listed_in_all(kind), kind, "{kind:?} is listed wrongly");
        }
        for (i, kind) in EffectKind::ALL.iter().enumerate() {
            assert!(
                !EffectKind::ALL[..i].contains(kind),
                "`EffectKind::ALL` lists {kind:?} twice, so a kind is missing"
            );
        }
    }

    /// [`OutlinePosition::ALL`]'s twin of the above, and it carries the same
    /// hole for the same reason.
    #[test]
    fn all_lists_every_outline_position() {
        const fn listed_in_all(position: OutlinePosition) -> OutlinePosition {
            match position {
                OutlinePosition::Outside => OutlinePosition::ALL[0],
                OutlinePosition::Centre => OutlinePosition::ALL[1],
                OutlinePosition::Inside => OutlinePosition::ALL[2],
            }
        }

        for position in OutlinePosition::ALL {
            assert_eq!(
                listed_in_all(position),
                position,
                "{position:?} is listed wrongly"
            );
        }
        for (i, position) in OutlinePosition::ALL.iter().enumerate() {
            assert!(
                !OutlinePosition::ALL[..i].contains(position),
                "`OutlinePosition::ALL` lists {position:?} twice, so one is missing"
            );
        }
    }

    /// The naming rule of the module docs, in the only two forms it can be
    /// checked in: the code says Outline and the interface says Stroke.
    #[test]
    fn the_interface_calls_an_outline_a_stroke() {
        assert_eq!(EffectKind::Outline.label(), "Stroke");
        assert_eq!(format!("{:?}", EffectKind::Outline), "Outline");
        assert_eq!(EffectKind::DropShadow.label(), "Drop shadow");
    }

    /// An effect goes to a file and comes back the same effect, colour
    /// included — awkward components rather than round ones, because RON writes
    /// an `f32` as text and a shortened one is a colour that moves a little
    /// every time a document is saved and reopened.
    #[test]
    fn an_effect_survives_its_own_round_trip() {
        let mut effect = Effect::outline();
        effect.color = Color::new(0.123_456_79, 0.007_812_5, 0.999_999_94, 0.333_333_34);
        effect.opacity = 0.618_034;
        effect.angle = 37.5;
        effect.position = OutlinePosition::Inside;
        effect.blend = BlendMode::Screen;

        let text = ron::to_string(&effect).unwrap();
        let back: Effect = ron::from_str(&text).unwrap();
        assert_eq!(back, effect, "written as {text}");
        assert_eq!(back.color.to_array(), effect.color.to_array());
    }

    /// **The field names are a file format too, and this is the guard that was
    /// missing.** `umber/effects/<n>.ron` carries every one of them, so a
    /// rename — `color` to `colour`, say, in a codebase whose stated convention
    /// is British spelling — changes what is written to disk exactly as
    /// renaming a variant does. It is *worse* than the variant case: the
    /// per-field `#[serde(default)]`s turn a field the reader does not
    /// recognise into a **silence**, so an old file loads with no error and the
    /// colour quietly gone.
    ///
    /// A round trip cannot see any of this — it is self-consistent under any
    /// rename — and neither can a fixture. Only text that does not move can.
    /// The `color:(…)` tuple pins [`linear_rgba`]'s shape at the same time:
    /// swapping it for four named components, or for sRGB bytes, fails here.
    #[test]
    fn an_effect_is_written_with_these_exact_field_names() {
        let text = ron::to_string(&Effect::outline()).unwrap();
        assert_eq!(
            text,
            "(kind:Outline,enabled:true,color:(0.0,0.0,0.0,1.0),opacity:1.0,\
             blend:Normal,spread:3.0,softness:0.0,angle:0.0,distance:0.0,\
             position:Outside)"
        );
    }

    /// A parameter added later must not make every effect already written
    /// unreadable — and the value an absent field takes must be **neutral**,
    /// not some other kind's.
    ///
    /// This test asserted the bug before a critic measured it: with a container
    /// `#[serde(default)]` filling from `Effect::default()`, this outline came
    /// back at the drop shadow's opacity, blend, softness and distance. It now
    /// reads what an outline that had never been told about those fields should
    /// read.
    #[test]
    fn an_effect_written_before_a_parameter_existed_loads_at_the_neutral() {
        let back: Effect = ron::from_str("(kind: Outline, spread: 8.0)").unwrap();
        assert_eq!(back.kind, EffectKind::Outline);
        assert_eq!(back.spread, 8.0);
        assert!(back.enabled, "an effect in the file is one somebody made");
        assert_eq!(back.opacity, 1.0);
        assert_eq!(back.blend, BlendMode::Normal);
        assert_eq!(back.color, Color::BLACK);
        assert_eq!(back.softness, 0.0, "not the drop shadow's 5.0");
        assert_eq!(back.distance, 0.0, "not the drop shadow's 5.0");
        assert_eq!(back.angle, 0.0);
        assert_eq!(back.position, OutlinePosition::Outside);

        // `spread` is the field the reading above *supplies*, so it is the one
        // field whose own default nothing else here can see — and an outline's
        // own starting width is 3.0, which is exactly the kind of value that
        // would look right and be another kind's.
        let bare: Effect = ron::from_str("(kind: Outline)").unwrap();
        assert_eq!(bare.spread, 0.0, "not the outline's own starting 3.0");
    }

    /// An effect with no kind is refused rather than read as an arbitrary one.
    ///
    /// The successful parse beside it is what makes the refusal `kind`'s: the
    /// two inputs differ in nothing else, so a later change that took some
    /// other field's default away could not leave this passing for the wrong
    /// reason. Asserting on the error's *wording* would be a promise about
    /// `ron`'s messages instead.
    #[test]
    fn an_effect_with_no_kind_is_not_read() {
        assert!(ron::from_str::<Effect>("(spread: 8.0)").is_err());
        assert!(ron::from_str::<Effect>("(kind: Outline, spread: 8.0)").is_ok());
    }

    /// **A `BlendMode`'s serde spelling reaches a document for the first time
    /// here, and nothing pinned it.**
    ///
    /// `docformat::blend_id` pins the `Debug` spelling, which `CLAUDE.md` says
    /// in as many words cannot see a `#[serde(rename)]`; and the only serde
    /// text anywhere is whatever a fixture happens to carry —
    /// `builtin-brushes.ron`'s 252 `blend:` fields are every one of them
    /// `Normal`, which is one data point wearing the costume of coverage. So
    /// renaming `Multiply` through serde left the whole suite green while
    /// changing what every `brushes.ron` carries; an [`Effect`] puts the same
    /// spelling in `umber/effects/<n>.ron`, and there an unknown variant is a
    /// hard parse error rather than a downgrade — the effect file does not
    /// load at all. Multiply is [`Effect::drop_shadow`]'s default, so it is the
    /// variant most likely to be in one.
    ///
    /// It lives here rather than beside the blend-mode tests in `docformat`
    /// because this module is what made it a document format. If those two are
    /// ever brought together, beside the enum is the better home for both.
    #[test]
    fn the_serialised_names_of_a_blend_mode_are_these_exact_strings() {
        let spelled: Vec<String> = BlendMode::ALL
            .into_iter()
            .map(|m| ron::to_string(&m).unwrap())
            .collect();
        assert_eq!(
            spelled,
            [
                "Normal",
                "Darken",
                "Multiply",
                "ColorBurn",
                "LinearBurn",
                "Lighten",
                "Screen",
                "ColorDodge",
                "Add",
                "Overlay",
                "SoftLight",
                "HardLight",
                "VividLight",
                "LinearLight",
                "PinLight",
                "Difference",
                "Exclusion",
                "Subtract",
                "Divide",
                "Hue",
                "Saturation",
                "Color",
                "Luminosity"
            ]
        );
    }

    /// §4's order, for every subset of the effects that exist.
    ///
    /// Two kinds and one of them in three positions, so the subsets are few
    /// enough to write out — which is worth more than a loop here, because what
    /// is being pinned is the *numbers* and a loop would have to restate them.
    #[test]
    fn effects_composite_in_the_order_the_design_gives() {
        let shadow = Effect::drop_shadow();
        let mut outside = Effect::outline();
        outside.position = OutlinePosition::Outside;
        let mut centre = Effect::outline();
        centre.position = OutlinePosition::Centre;
        let mut inside = Effect::outline();
        inside.position = OutlinePosition::Inside;

        assert_eq!(shadow.rank(), 1);
        assert_eq!(outside.rank(), 3);
        assert_eq!(centre.rank(), 3);
        assert_eq!(inside.rank(), 5);

        // A shadow and an outline, whichever way round they arrive.
        for pair in [[shadow, inside], [inside, shadow]] {
            let mut effects = pair;
            sort_into_composite_order(&mut effects);
            assert_eq!(effects, [shadow, inside]);
        }
        for pair in [[shadow, outside], [outside, shadow]] {
            let mut effects = pair;
            sort_into_composite_order(&mut effects);
            assert_eq!(effects, [shadow, outside]);
        }

        // Ordering one effect, or none, is the identity.
        let mut one = [inside];
        sort_into_composite_order(&mut one);
        assert_eq!(one, [inside]);
        sort_into_composite_order(&mut []);
    }

    /// **No effect may rank at [`LAYER_RANK`] itself, and this is the hole the
    /// exhaustive `match` in [`Effect::rank`] does not close.** That `match`
    /// forces a third kind to be given a *number*; it cannot force a legal one,
    /// and 4 is the most plausible slip because §4's own list — which the arms
    /// are written straight off — puts *the layer* at 4. An effect ranked there
    /// is silently neither inner nor outer: [`crate::Layer::effects_below`] and
    /// `effects_above` both skip it and it never draws at all. Exactly the
    /// silent-false failure `CLAUDE.md`'s "Partial exhaustiveness" section
    /// hunts, in numeric form rather than `matches!` form.
    ///
    /// Over both `ALL`s, so a kind or a position added is covered without this
    /// test being touched — unlike the hand-written list below it.
    #[test]
    fn no_effect_ranks_where_the_layer_does() {
        for kind in EffectKind::ALL {
            for position in OutlinePosition::ALL {
                let effect = Effect {
                    position,
                    ..Effect::of(kind)
                };
                assert_ne!(effect.rank(), LAYER_RANK, "{kind:?}/{position:?}");
                assert_ne!(
                    effect.is_inner(),
                    effect.is_outer(),
                    "{kind:?}/{position:?} is neither over nor under the layer"
                );
            }
        }
    }

    /// Which side of the layer each effect falls, which is what decides whether
    /// its confinement is baked or is `LayerDraw::clipped`.
    #[test]
    fn only_an_inside_outline_composites_over_the_layer() {
        let mut inside = Effect::outline();
        inside.position = OutlinePosition::Inside;
        assert!(inside.is_inner() && !inside.is_outer());

        for outer in [
            Effect::drop_shadow(),
            Effect::outline(),
            Effect {
                position: OutlinePosition::Centre,
                ..Effect::outline()
            },
        ] {
            assert!(outer.is_outer() && !outer.is_inner(), "{outer:?}");
        }
    }

    /// The angle convention, settled once so the bake cannot invent its own.
    #[test]
    fn the_shadow_falls_away_from_the_light() {
        // 120° is the default: light from the upper left, so the shadow goes
        // down and to the right in y-down document space.
        let shadow = Effect::drop_shadow();
        let (dx, dy) = shadow.offset();
        assert!(dx > 0.0, "{dx}");
        assert!(dy > 0.0, "{dy}");

        // Light from the right puts the shadow squarely to the left.
        let east = Effect {
            angle: 0.0,
            distance: 10.0,
            ..shadow
        };
        let (dx, dy) = east.offset();
        assert!((dx + 10.0).abs() < 1e-5, "{dx}");
        assert!(dy.abs() < 1e-5, "{dy}");

        // And the whole displacement is `distance` long, whatever the angle.
        for angle in [-400.0, -37.0, 0.0, 45.0, 120.0, 359.0, 720.0] {
            let (dx, dy) = Effect {
                angle,
                distance: 7.0,
                ..shadow
            }
            .offset();
            assert!((dx.hypot(dy) - 7.0).abs() < 1e-4, "{angle}");
        }

        // No distance is no offset, exactly.
        let still = Effect {
            distance: 0.0,
            ..shadow
        };
        assert_eq!(still.offset(), (0.0, 0.0));
    }

    /// A flip mirrors the lighting, and the displacement it produces is the
    /// mirror of the one it produced before.
    ///
    /// Stated over [`Effect::offset`] rather than over the angle, because the
    /// angle is a *spelling* of the displacement and the displacement is what the
    /// bake reads — an assertion on the number would pass for a convention that
    /// had been silently redefined. The default 120° is a light from the upper
    /// left, so the shadow falls down and to the right; mirrored horizontally it
    /// must fall down and to the *left*, and vertically up and to the right.
    #[test]
    fn flipping_a_canvas_mirrors_the_light_an_effect_falls_from() {
        use crate::geom::FlipAxis;

        let shadow = Effect {
            distance: 10.0,
            ..Effect::drop_shadow()
        };
        let (dx, dy) = shadow.offset();
        assert!(dx > 0.0 && dy > 0.0, "the default falls down and right");

        let (mx, my) = shadow.flipped(FlipAxis::Horizontal).offset();
        assert!((mx + dx).abs() < 1e-4, "{mx} against {dx}");
        assert!((my - dy).abs() < 1e-4, "{my} against {dy}");

        let (mx, my) = shadow.flipped(FlipAxis::Vertical).offset();
        assert!((mx - dx).abs() < 1e-4, "{mx} against {dx}");
        assert!((my + dy).abs() < 1e-4, "{my} against {dy}");

        // Every other field is a length or a colour, and a mirror preserves
        // those. Asserted so that a field added later is noticed here rather than
        // silently carried or silently mirrored.
        for axis in [FlipAxis::Horizontal, FlipAxis::Vertical] {
            let there = shadow.flipped(axis);
            assert_eq!(
                Effect {
                    angle: shadow.angle,
                    ..there
                },
                shadow,
                "a flip changed something other than the angle"
            );
        }
    }

    /// Flipping twice restores the angle — **bit for bit on one axis and within
    /// half an ulp of 180 on the other**, and the difference was measured rather
    /// than reasoned about.
    ///
    /// Undoing a flip *is* another flip — the reason `flip.wgsl` is an exact texel
    /// permutation — so anything lost here compounds every time somebody flips and
    /// undoes, and there is no other route back. So the requirement is exactness,
    /// and this test is where the honest answer had to replace the expected one.
    ///
    /// **The vertical axis is `-a`, a negation, and is exact for every `f32` there
    /// is.** The horizontal axis is `180 - a` and is *not*: by Sterbenz the
    /// subtraction is exact for `a` in `[90, 360]`, and outside that it rounds —
    /// `180.0 - (180.0 - 0.1)` is `0.100006`. Naming this test "exactly" and
    /// asserting a tolerance would be `CLAUDE.md`'s "a guard's comment must not
    /// claim more reach than the mutation demonstrates", so the name says what is
    /// true and the bound is **derived** rather than picked: the error is at most
    /// half an ulp of 180, which is `180 · f32::EPSILON / 2`, and the assertion
    /// uses the whole ulp for slack.
    ///
    /// **It does not compound.** `f(a) = 180 - a` is its own near-inverse, so a
    /// pair of flips adds at most one ulp and a thousand of them add at most five
    /// hundred — 0.008 of a degree, which over the longest displacement a canvas
    /// can hold is a hundredth of a pixel. That is the reason a bounded error is
    /// acceptable here where a bounded error in `flip.wgsl` would not be: the
    /// shader's would be re-quantised into eight bits every time.
    ///
    /// An exact horizontal mirror **is** available and was rejected: negate the
    /// angle *and* the distance, both of which are exact, and the displacement
    /// comes out identical. It makes `distance` negative, which
    /// [`effect_marks_nothing`](crate::effect) reads as no reach — so a hard offset
    /// shadow would stop drawing the moment somebody flipped the canvas — and it
    /// puts a sign convention nobody asked for into the file and onto the panel's
    /// own readout. A hundredth of a pixel is the better trade.
    ///
    /// The sweep carries angles the interface can produce and a file can hold:
    /// tenths, values either side of Sterbenz's boundary, a value near zero, and
    /// values past a full turn in both directions.
    #[test]
    fn flipping_a_canvas_twice_restores_an_effects_angle() {
        use crate::geom::FlipAxis;

        // One ulp of 180 in `f32`: 1.526e-5 of a degree.
        let slack = 180.0 * f32::EPSILON;

        for angle in [
            0.0, 0.1, -0.1, 1.0, 37.5, 45.0, 89.9, 90.0, 120.0, 179.9, 180.0, 270.0, 359.9, 360.0,
            720.0, -400.0, 1e-6,
        ] {
            let effect = Effect {
                angle,
                distance: 1000.0,
                ..Effect::drop_shadow()
            };

            let back = effect
                .flipped(FlipAxis::Vertical)
                .flipped(FlipAxis::Vertical);
            assert_eq!(back.angle, angle, "vertical is a negation, so at {angle}");

            let back = effect
                .flipped(FlipAxis::Horizontal)
                .flipped(FlipAxis::Horizontal);
            assert!(
                (back.angle - angle).abs() <= slack,
                "horizontal, at {angle}: came back {} , which is over one ulp of \
                 180 out",
                back.angle
            );
            // And the displacement it produces with it, because the angle is a
            // spelling of that and a bound on one is not a bound on the other.
            // A thousand pixels of displacement, so this is in the same units a
            // canvas is.
            let ((ax, ay), (bx, by)) = (effect.offset(), back.offset());
            assert!(
                (ax - bx).abs() < 1e-3 && (ay - by).abs() < 1e-3,
                "horizontal, at {angle}: ({ax}, {ay}) became ({bx}, {by})"
            );
        }
    }

    /// The budget's boundary as arithmetic. `layer.rs`'s tests are what meet it
    /// through the real gate, on a document somebody could build — see
    /// [`MAX_ENABLED`], which is live rather than aspirational.
    #[test]
    fn the_budget_admits_exactly_its_own_figure_and_no_more() {
        assert!(within_budget(0));
        assert!(within_budget(MAX_ENABLED - 1));
        assert!(within_budget(MAX_ENABLED));
        assert!(!within_budget(MAX_ENABLED + 1));
    }

    /// [`Effect`] is `Copy`, which is the whole of why `docs/layer-effects.md`
    /// §13's question about `Layer: Clone` does not arise. A compile-time
    /// assertion, because that is the only kind there is for a trait bound.
    #[test]
    fn an_effect_is_copy_and_needs_no_allocation() {
        const fn assert_copy<T: Copy>() {}
        assert_copy::<Effect>();
        assert_copy::<EffectKind>();
        assert_copy::<OutlinePosition>();
    }
}
