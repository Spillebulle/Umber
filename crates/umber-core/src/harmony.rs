//! Colour harmony: the hues related to one the artist has chosen.
//!
//! A harmony is a function of **hue alone**, which is the whole of why it lives
//! here rather than in the picker that draws it. The picker's source of truth
//! is [`crate::Hsv`] — hue is undefined for greys, so deriving it from the
//! colour each frame would silently reset it to red the moment value reached
//! zero — and a harmony reads that hue and answers with more of them. Nothing
//! in here knows what a wheel looks like.
//!
//! Saturation and value are deliberately **carried across unchanged**. A
//! complementary of a pale grey-blue is a pale grey-orange, not a saturated
//! one: what the relation names is the angle round the wheel, and altering the
//! other two axes on the way would be the module inventing a colour nobody
//! asked for. The picker applies the artist's own `s` and `v` to every hue this
//! returns, so a harmony of a grey is a row of identical greys — correct, and is
//! also why the picker draws the base swatch beside the rest rather than
//! relying on the ring to tell them apart.
//!
//! ## Which wheel these angles are on
//!
//! The **RGB wheel**, because that is what [`crate::Hsv`]'s hue is: 0° is red,
//! 120° green, 240° blue. So the complement of blue here is *yellow* and the
//! complement of red is *cyan*. On the painter's RYB wheel taught as colour
//! theory, blue's complement is orange and red's is green. Both are defensible
//! and they are visibly different answers, so it is worth saying plainly which
//! one this is and why it is not going to change quietly.
//!
//! The offsets below are right **for the wheel they are stated on** — 180° is
//! genuinely the opposite hue, a triad is genuinely three equal thirds — and
//! that is the only sense in which a set of angles can be right. Reading them
//! as RYB angles is what would be wrong.
//!
//! It also matches every painting application that draws a hue wheel. Krita's
//! selectors are HSV/HSL/HSI/HSY' over RGB and offer no harmony rules at all
//! (gamut masks instead); Clip Studio's Color Wheel palette is HSV or HLS with
//! no relation generator; Photoshop has had none in the application since the
//! Adobe Color Themes panel was withdrawn. The one mainstream tool that
//! computes harmonies on RYB is **Adobe Color**, a web tool rather than a
//! painting application, and it converts RGB to RYB and back for exactly this
//! — which is why its complement of red comes out near hue 137 rather than 180
//! and reads as an error to anybody checking the arithmetic.
//!
//! **An RYB mode would be a real feature and is not a bug fix.** It needs a
//! control, because a painter who wants one wheel does not want the other by
//! surprise; it may not change what any existing preset or document means, and
//! nothing here reaches a file today; and it cannot be a second `hues` beside
//! this one, because the whole module would then have two answers to one
//! question. The shape it would take is a hue *mapping* on the way in and out —
//! RGB hue to RYB, the offsets applied there, RYB back to RGB — leaving these
//! angles and every caller untouched. Nobody has asked for it twice yet.

/// One of the relations the design's Harmony mode names.
///
/// Each is a *rule* about the wheel that a painter can hold in their head. One
/// that was only a different number of degrees would be a row in a menu rather
/// than something anybody would reach for — which is why the two tetrads are
/// both here and a "double complementary at 45°" is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Harmony {
    /// The hue and its opposite.
    #[default]
    Complementary,
    /// The hue and its two near neighbours.
    Analogous,
    /// Three hues an equal third of the wheel apart.
    Triad,
    /// The opposite hue, split into the two either side of it. Softer than a
    /// straight complementary, which is the reason it exists as its own row.
    SplitComplementary,
    /// Four hues a quarter of the wheel apart — the *square* tetrad.
    ///
    /// **The variant keeps the bare name and the label does not**, and that is
    /// deliberate rather than an oversight. `Harmony::Tetrad` is what a
    /// preferences file has been writing as `tetrad` since this enum existed,
    /// so renaming it would either change what such a file means or leave a
    /// variant and its id spelling different things — the trap
    /// `docformat`'s "a derived spelling that reaches a file is a format"
    /// rule already names. The *label* is free to move, and had to: with
    /// [`Self::RectangleTetrad`] beside it a bare "Tetrad" no longer says
    /// which of the two was drawn.
    Tetrad,
    /// Four hues as two complementary pairs 60° apart — the *rectangle* tetrad,
    /// also called the double complementary.
    ///
    /// A genuinely different set from the square, not a rotation of it: the
    /// square is four hues nobody can pair off, where this is two
    /// complementaries chosen to sit near each other, so one pair dominates and
    /// the other accents it. That is the whole reason a painter reaches for it,
    /// and it is why the two cannot share a row.
    RectangleTetrad,
}

/// How many hues the widest harmony has, including the base.
///
/// [`Hues`] is a fixed array of this size rather than a `Vec` because the
/// picker asks for one every frame it is drawn, and four floats on the stack is
/// not something to allocate for.
pub const MAX_HUES: usize = 4;

impl Harmony {
    pub const ALL: [Harmony; 6] = [
        Self::Complementary,
        Self::Analogous,
        Self::Triad,
        Self::SplitComplementary,
        Self::Tetrad,
        Self::RectangleTetrad,
    ];

    /// The name the picker draws.
    ///
    /// Both tetrads are qualified, and neither may be left bare. The word names
    /// two different sets of four hues, so a row reading "Tetrad" beside one
    /// reading "Tetrad (rectangle)" would be a control lying about which of
    /// them it drew — and the two are one dropdown row apart, which is exactly
    /// where nobody would notice. They share a first word so the list groups
    /// them without needing a heading.
    pub fn label(self) -> &'static str {
        match self {
            Self::Complementary => "Complementary",
            Self::Analogous => "Analogous",
            Self::Triad => "Triad",
            Self::SplitComplementary => "Split complementary",
            Self::Tetrad => "Tetrad (square)",
            Self::RectangleTetrad => "Tetrad (rectangle)",
        }
    }

    /// Degrees round the wheel from the chosen hue.
    ///
    /// **The first is always zero**, so the colour the artist already has is the
    /// first thing the picker draws and every caller can rely on index 0 being
    /// theirs rather than having to search for it.
    pub fn offsets(self) -> &'static [f32] {
        match self {
            Self::Complementary => &[0.0, 180.0],
            // Thirty either side. Twelve o'clock on a twelve-hue wheel is the
            // step painters mixing by eye actually use, and a wider spread stops
            // reading as a neighbour at all.
            Self::Analogous => &[0.0, -30.0, 30.0],
            Self::Triad => &[0.0, 120.0, 240.0],
            // 180 ± 30, and deliberately the same thirty the analogous set
            // uses: the two are the same gesture read from opposite ends of the
            // wheel, and a second number would be two rules to remember.
            Self::SplitComplementary => &[0.0, 150.0, 210.0],
            Self::Tetrad => &[0.0, 90.0, 180.0, 270.0],
            // Two complementary pairs — 0/180 and 60/240 — and sixty is the
            // gap between them rather than a fourth arbitrary number: it is
            // the analogous step doubled, which is the widest separation that
            // still reads as one pair leaning on another rather than as the
            // square. Listed in order round the wheel, so the swatch row runs
            // the way the markers do.
            Self::RectangleTetrad => &[0.0, 60.0, 180.0, 240.0],
        }
    }

    /// The hues this harmony reaches from `base`, wrapped into `0.0..360.0`.
    pub fn hues(self, base: f32) -> Hues {
        let offsets = self.offsets();
        let mut values = [0.0; MAX_HUES];
        let base = crate::color::wrap_hue(base);
        for (slot, offset) in values.iter_mut().zip(offsets) {
            *slot = crate::color::wrap_hue(base + offset);
        }
        Hues {
            values,
            len: offsets.len(),
        }
    }
}

/// A harmony's hues: at most [`MAX_HUES`] of them, the chosen one first.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hues {
    values: [f32; MAX_HUES],
    len: usize,
}

impl Hues {
    pub fn as_slice(&self) -> &[f32] {
        &self.values[..self.len]
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Index 0 is the artist's own hue, in every harmony. The picker draws it
    /// first and reads it back by position, so this is the promise `offsets`
    /// makes in its own documentation.
    #[test]
    fn every_harmony_starts_at_the_hue_it_was_given() {
        for harmony in Harmony::ALL {
            assert_eq!(harmony.offsets()[0], 0.0, "{}", harmony.label());
            for base in [0.0, 37.0, 180.0, 359.9] {
                assert_eq!(
                    harmony.hues(base).as_slice()[0],
                    base,
                    "{} at {base}",
                    harmony.label()
                );
            }
        }
    }

    /// Every hue a harmony produces has to be a hue: `Hsv` takes degrees in
    /// `0..360`, and a value outside that reaches `sin_cos` in the picker's
    /// mesh. One NaN vertex is a mesh egui discards whole — a picker that has
    /// silently stopped drawing.
    #[test]
    fn every_hue_is_inside_a_single_turn() {
        for harmony in Harmony::ALL {
            for base in [0.0, 1.0, 179.0, 350.0, 359.999, -90.0, 720.0] {
                for hue in harmony.hues(base).as_slice() {
                    assert!(
                        hue.is_finite() && (0.0..360.0).contains(hue),
                        "{} at {base} produced {hue}",
                        harmony.label()
                    );
                }
            }
        }
    }

    /// A base nothing sensible produced still has to come back as a hue, for
    /// the reason above. `Hsv::h` is a plain public field, so this is reachable
    /// from anything that writes one.
    #[test]
    fn a_hue_that_is_not_a_number_becomes_one() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            for harmony in Harmony::ALL {
                for hue in harmony.hues(bad).as_slice() {
                    assert!(hue.is_finite(), "{} kept {bad}", harmony.label());
                }
            }
        }
    }

    /// The wheel these angles are on is the RGB one, and that is a decision
    /// rather than an accident — see the module docs.
    ///
    /// Pinned as the three pairs somebody would actually check by eye, because
    /// "the complement of blue is yellow" is the sentence an RYB reading would
    /// make false, and a change to RYB that left the offsets alone would leave
    /// every other test in this file green.
    #[test]
    fn a_complement_is_the_opposite_hue_on_the_rgb_wheel() {
        let opposite = |hue: f32| Harmony::Complementary.hues(hue).as_slice()[1];
        assert_eq!(
            opposite(240.0),
            60.0,
            "blue's complement is yellow, not orange"
        );
        assert_eq!(opposite(0.0), 180.0, "red's complement is cyan, not green");
        assert_eq!(opposite(120.0), 300.0, "green's complement is magenta");
    }

    /// The relations are what their names say. Checked as *angles*, because
    /// that is the only thing a harmony is.
    #[test]
    fn the_relations_are_the_angles_their_names_promise() {
        assert_eq!(Harmony::Complementary.offsets(), &[0.0, 180.0]);
        // A triad is an equal third of the wheel, all three of them.
        assert_eq!(Harmony::Triad.hues(0.0).as_slice(), &[0.0, 120.0, 240.0]);
        // Analogous is symmetric about the base, and the split complementary is
        // symmetric about the base's opposite.
        assert_eq!(
            Harmony::Analogous.hues(90.0).as_slice(),
            &[90.0, 60.0, 120.0]
        );
        assert_eq!(
            Harmony::SplitComplementary.hues(0.0).as_slice(),
            &[0.0, 150.0, 210.0]
        );
        assert_eq!(
            Harmony::Tetrad.hues(0.0).as_slice(),
            &[0.0, 90.0, 180.0, 270.0]
        );
        // The rectangle is two complementary pairs, which is the property that
        // tells it from the square and the only one worth asserting: checking
        // the four numbers alone would pass for any four.
        let rect = Harmony::RectangleTetrad.hues(0.0);
        assert_eq!(rect.as_slice(), &[0.0, 60.0, 180.0, 240.0]);
        for (a, b) in [(0, 2), (1, 3)] {
            let apart = (rect.as_slice()[b] - rect.as_slice()[a]).rem_euclid(360.0);
            assert!(
                (apart - 180.0).abs() < 1e-3,
                "{a} and {b} are {apart} apart"
            );
        }
    }

    /// The two tetrads are different *sets*, not one drawn from two headings.
    ///
    /// A rotation would make them the same relation under two names, which is
    /// the row nobody would ever reach for twice — and it is the failure a
    /// label alone cannot catch, because both labels would be true of both
    /// sets. Checked as an unordered set at every base, since "the same four
    /// hues in a different order" is still the same harmony.
    #[test]
    fn the_two_tetrads_are_not_the_same_four_hues() {
        let sorted = |harmony: Harmony, base: f32| {
            let mut hues = harmony.hues(base).as_slice().to_vec();
            hues.sort_by(f32::total_cmp);
            hues
        };
        for base in [0.0, 37.0, 90.0, 200.0, 359.0] {
            assert_ne!(
                sorted(Harmony::Tetrad, base),
                sorted(Harmony::RectangleTetrad, base),
                "at {base}"
            );
        }
    }

    /// No relation names one hue twice.
    ///
    /// A repeated member draws two identical swatches, one of which is dead —
    /// the picker marks the base and makes the rest clickable, so a duplicate
    /// would be a "take this colour" that hands back the colour already in
    /// hand. It is also the shape a mistyped offset takes: 240 written where
    /// 180 was meant is a legal-looking set with a hole in it.
    #[test]
    fn no_relation_names_the_same_hue_twice() {
        for harmony in Harmony::ALL {
            let hues = harmony.hues(17.0);
            let hues = hues.as_slice();
            for (i, a) in hues.iter().enumerate() {
                for b in &hues[i + 1..] {
                    let apart = (b - a).rem_euclid(360.0).min((a - b).rem_euclid(360.0));
                    assert!(apart > 1.0, "{} repeats {a}", harmony.label());
                }
            }
        }
    }

    /// Both tetrads are named as tetrads, and neither is left bare.
    ///
    /// The one property of the labels that can be wrong in a way nobody sees:
    /// the two sets are adjacent rows of one dropdown, so a bare "Tetrad"
    /// beside "Tetrad (rectangle)" reads as *the* tetrad rather than as the
    /// square one. Pinned as text because the label is what a painter reads.
    #[test]
    fn neither_tetrad_is_called_only_a_tetrad() {
        assert_eq!(Harmony::Tetrad.label(), "Tetrad (square)");
        assert_eq!(Harmony::RectangleTetrad.label(), "Tetrad (rectangle)");
    }

    /// A harmony is a rotation, so turning the base turns the whole set by the
    /// same amount — the relation between the members cannot depend on where
    /// the artist happens to be standing.
    #[test]
    fn turning_the_base_turns_the_whole_set() {
        for harmony in Harmony::ALL {
            let at_zero = harmony.hues(0.0);
            let turned = harmony.hues(47.0);
            for (a, b) in at_zero.as_slice().iter().zip(turned.as_slice()) {
                let moved = (b - a).rem_euclid(360.0);
                assert!(
                    (moved - 47.0).abs() < 1e-3,
                    "{} moved {moved} rather than 47",
                    harmony.label()
                );
            }
        }
    }

    /// A wheel can only draw what it is handed room for.
    #[test]
    fn no_harmony_is_wider_than_the_array_that_holds_it() {
        for harmony in Harmony::ALL {
            assert!(harmony.offsets().len() <= MAX_HUES, "{}", harmony.label());
            assert_eq!(harmony.hues(0.0).len(), harmony.offsets().len());
            assert!(!harmony.hues(0.0).is_empty());
        }
    }
}
