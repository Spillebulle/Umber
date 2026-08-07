//! Layer effects: the parameters, the order they composite in, and the cap.
//!
//! Non-destructive marks derived from a layer's own alpha and composited around
//! it — a stroke, a drop shadow. The layer's pixels are not touched, the
//! parameters stay editable, and the file carries the parameters rather than the
//! result. `docs/layer-effects.md` is the design and has the whole argument.
//!
//! **This is stage 0 and it is inert.** Nothing here produces a draw, allocates
//! a slice or reaches a shader; what it is for is that everything structural —
//! the parameter set, the ordering, the refusals and the serialised spelling —
//! is settled and testable before a single pass is written. A document with no
//! effects produces exactly the draw list it produced before, entry for entry,
//! because nothing reads any of this yet.
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
//! or function under `umber-core` or `umber-render` may spell it "Stroke"**. The
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
/// `docs/layer-effects.md` §6.2's arithmetic: the composite's uniform array is
/// to be sized at `MAX_DRAWS = 192` entries, `LayerStack::MAX` stays 64, and the
/// difference is what is left for effect draws. Those two numbers live in
/// `canvas.rs` and `composite.wgsl` and **all three have to agree** — a
/// truncated draw list is the one outcome that must stay unreachable, because a
/// list cut off mid-group leaves an accumulator open.
///
/// A *disabled* effect costs nothing: it produces no draw, so it is not counted.
///
/// **Nothing can reach this today and that is not an oversight.** There are two
/// kinds and a layer holds at most one of each, so a full stack of 64 layers is
/// exactly 128 enabled effects — the cap, met and never exceeded. It is written
/// and enforced anyway, because the moment a third kind lands the ceiling is 192
/// and the refusal is live; `the_effect_budget_is_exactly_reachable_and_no_more`
/// fails the build at that point so that whoever adds the kind reads this.
pub const MAX_ENABLED: usize = 128;

/// Where the layer's own draw sits in `docs/layer-effects.md` §4's numbering.
///
/// An effect whose [`Effect::rank`] is below this composites **under** the
/// layer, one above it **over**.
pub const LAYER_RANK: u8 = 4;

/// Is a document holding `enabled` enabled effects within its budget?
///
/// A free function so that the boundary is a test rather than a sentence, for
/// the reason `widgets::track_value` is one: the arithmetic is trivial and the
/// case that matters is the one nothing can currently construct.
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
/// Every field carries `#[serde(default)]` through the container attribute, for
/// the reason [`crate::Brush`]'s do: a file written before a parameter existed
/// still loads. The cost is that a file omitting `kind` reads as the default's
/// kind rather than being refused, which is worth naming because it is the one
/// field where the default is arbitrary — Umber always writes it, and a reader
/// wanting to refuse a file that does not is welcome to check before it gets
/// here.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Effect {
    pub kind: EffectKind,
    /// Off means no draw and no bake, and therefore nothing charged against
    /// [`MAX_ENABLED`]. The parameters are kept, so switching it back on gives
    /// back the effect that was dialled rather than a fresh one.
    pub enabled: bool,
    /// Written down by [`linear_rgba`], which is the one statement of an effect
    /// colour's serialised form.
    #[serde(with = "linear_rgba")]
    pub color: Color,
    /// `0.0..=1.0`, applied to the effect's own draw.
    pub opacity: f32,
    /// How the effect's draw combines with what is under it.
    ///
    /// **With the backdrop, not with its own layer** — that is the whole point
    /// of an effect being a draw of its own rather than pixels baked into the
    /// layer's slice, and it is what makes a drop shadow at Multiply do what
    /// Photoshop's default does.
    pub blend: BlendMode,
    /// How far the shape is grown before it is softened, in document pixels.
    ///
    /// The outline's width; Photoshop's *Spread* on a shadow.
    pub spread: f32,
    /// Blur radius, in document pixels. Photoshop's *Size*.
    ///
    /// **Zero is the exact identity** — the rule the selection's feather and the
    /// brush's grain both keep, and the one the bake has to hold to.
    pub softness: f32,
    /// Where the light is, in degrees anticlockwise from the right.
    ///
    /// Photoshop's and Krita's convention, and the one a dial in the interface
    /// will show. The shadow falls *away* from it — see [`Effect::offset`],
    /// which is the only place that is worked out.
    pub angle: f32,
    /// How far the shadow is displaced, in document pixels.
    pub distance: f32,
    /// Read by [`EffectKind::Outline`] alone, and carried by every effect for
    /// the reason the module docs give.
    pub position: OutlinePosition,
}

impl Default for Effect {
    fn default() -> Self {
        Self::drop_shadow()
    }
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
    /// An outer effect, in `docs/layer-effects.md` §3.3's sense: its confinement
    /// is *baked* — the bake multiplies it by `1 − coverage`, so it cannot paint
    /// under the layer's own opaque pixels — because doing it at composite time
    /// would need an inverse clip the shader has no notion of.
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
/// Nothing outside this crate should have to call it: [`crate::Layer`] holds its
/// effects in this order at all times, and that invariant is maintained at the
/// one gate that writes them rather than by the callers.
pub fn sort_into_composite_order(effects: &mut [Effect]) {
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

    /// The container `#[serde(default)]` is what lets a parameter be added later
    /// without every effect already written becoming unreadable.
    #[test]
    fn an_effect_written_before_a_parameter_existed_still_loads() {
        let back: Effect = ron::from_str("(kind: Outline, spread: 8.0)").unwrap();
        assert_eq!(back.kind, EffectKind::Outline);
        assert_eq!(back.spread, 8.0);
        assert_eq!(back.opacity, Effect::default().opacity);
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

    /// The budget's boundary, which nothing in the model can currently reach —
    /// see [`MAX_ENABLED`]. Tested here as arithmetic so that the day a third
    /// effect kind makes it reachable, the rule it will be enforced by has
    /// already been checked.
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
