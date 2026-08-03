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
//! returns, so a harmony of a grey is five greys — which is correct, and is
//! also why the picker draws the base swatch beside the rest rather than
//! relying on the ring to tell them apart.

/// One of the five relations the design's Harmony mode names.
///
/// Five and not more: each is a *rule* about the wheel that a painter can hold
/// in their head, and a sixth that was only a different number of degrees would
/// be a row in a menu rather than something anybody would reach for.
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
    /// The other reading of the word is the rectangle (0°, 60°, 180°, 240°),
    /// which is a different set. Only one of them can be called Tetrad without
    /// the label lying about which it drew, and the square is the one every
    /// wheel-based picker means by the bare word.
    Tetrad,
}

/// How many hues the widest harmony has, including the base.
///
/// [`Hues`] is a fixed array of this size rather than a `Vec` because the
/// picker asks for one every frame it is drawn, and four floats on the stack is
/// not something to allocate for.
pub const MAX_HUES: usize = 4;

impl Harmony {
    pub const ALL: [Harmony; 5] = [
        Self::Complementary,
        Self::Analogous,
        Self::Triad,
        Self::SplitComplementary,
        Self::Tetrad,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Complementary => "Complementary",
            Self::Analogous => "Analogous",
            Self::Triad => "Triad",
            Self::SplitComplementary => "Split complementary",
            Self::Tetrad => "Tetrad",
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

    /// The relations are what their names say. Checked as *angles*, because
    /// that is the only thing a harmony is.
    #[test]
    fn the_relations_are_the_angles_their_names_promise() {
        assert_eq!(Harmony::Complementary.offsets(), &[0.0, 180.0]);
        // A triad is an equal third of the wheel, all three of them.
        assert_eq!(Harmony::Triad.hues(0.0).as_slice(), &[0.0, 120.0, 240.0]);
        // Analogous is symmetric about the base, and the split complementary is
        // symmetric about the base's opposite.
        assert_eq!(Harmony::Analogous.hues(90.0).as_slice(), &[90.0, 60.0, 120.0]);
        assert_eq!(
            Harmony::SplitComplementary.hues(0.0).as_slice(),
            &[0.0, 150.0, 210.0]
        );
        assert_eq!(
            Harmony::Tetrad.hues(0.0).as_slice(),
            &[0.0, 90.0, 180.0, 270.0]
        );
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
