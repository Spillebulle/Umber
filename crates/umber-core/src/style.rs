//! Sorting brushes by the kind of mark they make.
//!
//! A brush pack arrives grouped by whoever drew it. That is the right way to
//! *credit* a pack and the wrong way to *browse* one: nobody reaches for a
//! brush by remembering which artist contributed it, and a library sorted that
//! way puts the pencils in six different places. Umber therefore groups by
//! style and keeps the authorship on the brush, in [`crate::preset::Credit`],
//! where the browser shows it on every row.
//!
//! # How a brush is classified
//!
//! Erasing decides it outright: an eraser named "Knife" is an eraser, and
//! filing it under paint would be actively misleading. Everything else goes by
//! name, which in every pack worth shipping describes the medium —
//! `charcoal-03`, `watercolor-02-paint`, `marker_fat` — and only falls back to
//! the settings when the name says nothing.
//!
//! Smudge is deliberately *not* decisive, which is worth explaining because the
//! obvious design gets it badly wrong. Real oil and acrylic brushes mix with
//! what is already on the canvas: more than a third of the MyPaint set has
//! `smudge >= 0.5`, including every oil paint in it. Sorting on that put 68 of
//! 196 brushes in one bin and emptied the paint collection. A brush that
//! smudges is only a *blender* when blending is the whole point of it, and the
//! name is what says so.
//!
//! Order matters and is deliberate. `charcoal-blur1` is a blender that happens
//! to be made of charcoal, `smudge_ink` is a blender that happens to be made of
//! ink, and `acrylic-03-with-water` is wet media rather than acrylic. The
//! earlier a rule sits in [`RULES`], the more it dominates — so the rules read
//! from most specific to least.
//!
//! An unrecognised brush lands in [`Style::PAINT`] rather than in a bin called
//! "other". Most brushes in most packs are some kind of round painting brush,
//! and a category the user is invited to ignore is worse than a slightly
//! over-full one they will actually open.

use crate::brush::{Brush, BrushMode};

/// The categories, in the order the picker should list them.
///
/// Roughly the order a painter works in — drawing media, then paint, then the
/// things done to paint already on the canvas, then the specialities.
pub struct Style;

impl Style {
    pub const PENCIL: &'static str = "Pencils & sketching";
    pub const INK: &'static str = "Inks & pens";
    pub const MARKER: &'static str = "Markers";
    pub const CHARCOAL: &'static str = "Charcoal, chalk & pastel";
    pub const PAINT: &'static str = "Paint & brushes";
    pub const WATERCOLOUR: &'static str = "Watercolour & wet media";
    pub const AIRBRUSH: &'static str = "Airbrush & spray";
    pub const BLENDER: &'static str = "Blenders & smudge";
    pub const ERASER: &'static str = "Erasers";
    pub const TEXTURE: &'static str = "Texture & grain";
    pub const NATURE: &'static str = "Foliage & fur";
    pub const EFFECT: &'static str = "Effects & experimental";

    /// Every category, in listing order. The picker walks this so a collection
    /// that happens to be empty in one library still sorts where it belongs.
    pub const ALL: [&'static str; 12] = [
        Self::PENCIL,
        Self::INK,
        Self::MARKER,
        Self::CHARCOAL,
        Self::PAINT,
        Self::WATERCOLOUR,
        Self::AIRBRUSH,
        Self::BLENDER,
        Self::ERASER,
        Self::TEXTURE,
        Self::NATURE,
        Self::EFFECT,
    ];
}

/// Name fragments and the style they imply, most specific first.
///
/// Matched against the name lowercased with separators folded away, so
/// `8B_Pencil#1`, `pencil-8b` and `subtle pencil` all meet the same rule.
const RULES: &[(&str, &str)] = &[
    // --- things done to paint that is already there ------------------------
    // Ahead of every medium: `charcoal-blur1` and `smudge_ink` are blenders
    // made of charcoal and ink, not charcoals and inks that happen to blend.
    ("smudg", Style::BLENDER),
    ("smear", Style::BLENDER),
    ("blend", Style::BLENDER),
    ("blur", Style::BLENDER),
    ("pickanddrag", Style::BLENDER),
    ("dissolver", Style::BLENDER),
    // --- wet media ---------------------------------------------------------
    // Also ahead of the paints, so `acrylic-03-with-water` and
    // `oil-01-clean`'s watery siblings sort by how they behave.
    ("watercolor", Style::WATERCOLOUR),
    ("watercolour", Style::WATERCOLOUR),
    ("withwater", Style::WATERCOLOUR),
    ("onlywater", Style::WATERCOLOUR),
    ("wet", Style::WATERCOLOUR),
    ("water", Style::WATERCOLOUR),
    // --- drawing media -----------------------------------------------------
    ("pencil", Style::PENCIL),
    ("sketch", Style::PENCIL),
    ("graphite", Style::PENCIL),
    ("charcoal", Style::CHARCOAL),
    ("chalk", Style::CHARCOAL),
    ("pastel", Style::CHARCOAL),
    ("conte", Style::CHARCOAL),
    ("marker", Style::MARKER),
    // --- pens and ink ------------------------------------------------------
    ("ink", Style::INK),
    ("pen", Style::INK),
    ("calligraphy", Style::INK),
    ("liner", Style::INK),
    ("rigger", Style::INK),
    ("nib", Style::INK),
    ("kabura", Style::INK),
    ("fount", Style::INK),
    // --- sprayed -----------------------------------------------------------
    ("airbrush", Style::AIRBRUSH),
    ("airbruch", Style::AIRBRUSH), // the pack's own spelling
    ("spray", Style::AIRBRUSH),
    ("splatter", Style::AIRBRUSH),
    ("splash", Style::AIRBRUSH),
    // --- surfaces ----------------------------------------------------------
    ("texture", Style::TEXTURE),
    ("grain", Style::TEXTURE),
    ("noise", Style::TEXTURE),
    ("rough", Style::TEXTURE),
    ("bulk", Style::TEXTURE),
    ("sponge", Style::TEXTURE),
    ("dirty", Style::TEXTURE),
    ("coarse", Style::TEXTURE),
    // --- shapes that are pictures of something -----------------------------
    ("feather", Style::NATURE),
    ("grass", Style::NATURE),
    ("leaves", Style::NATURE),
    ("leaf", Style::NATURE),
    ("fur", Style::NATURE),
    ("cloud", Style::NATURE),
    // --- named effects -----------------------------------------------------
    // Above the paints, because these are distinctive names while `brush` and
    // `paint` below are catch-alls: `DNA_brush` is an effect, not a brush, and
    // whichever of the two rules comes first decides that.
    ("beamlight", Style::EFFECT),
    ("halftone", Style::EFFECT),
    ("posteriz", Style::EFFECT),
    ("puantilism", Style::EFFECT),
    ("pointilism", Style::EFFECT),
    ("particule", Style::EFFECT),
    ("particle", Style::EFFECT),
    ("pixel", Style::EFFECT),
    ("dna", Style::EFFECT),
    ("dissol", Style::EFFECT),
    ("sewing", Style::EFFECT),
    ("track", Style::EFFECT),
    ("arrow", Style::EFFECT),
    ("delayed", Style::EFFECT),
    ("bubble", Style::EFFECT),
    ("glow", Style::EFFECT),
    ("fill", Style::EFFECT),
    ("blot", Style::INK),
    ("sting", Style::INK),
    // --- paint -------------------------------------------------------------
    // Last, and generic. `brush`, `paint`, `round` and `soft` appear inside
    // dozens of names that mean something more specific, so every rule that
    // means something more specific has to sit above these.
    ("acrylic", Style::PAINT),
    ("oil", Style::PAINT),
    ("gouache", Style::PAINT),
    ("impasto", Style::PAINT),
    ("impression", Style::PAINT),
    ("impdetail", Style::PAINT),
    ("knife", Style::PAINT),
    ("bristle", Style::PAINT),
    ("flat", Style::PAINT),
    ("fan", Style::PAINT),
    ("paint", Style::PAINT),
    ("brush", Style::PAINT),
    ("round", Style::PAINT),
    ("shade", Style::PAINT),
];

/// Which collection this brush belongs in.
pub fn classify(name: &str, brush: &Brush) -> &'static str {
    // What the brush does beats what it is called. An eraser named "Knife" is
    // still an eraser, and filing it under paint would be actively misleading.
    if brush.mode == BrushMode::Erase {
        return Style::ERASER;
    }

    let folded = fold(name);
    if let Some((_, style)) = RULES.iter().find(|(needle, _)| folded.contains(needle)) {
        return style;
    }

    // Nothing in the name. Now the settings are the only evidence there is.
    //
    // A brush that deposits paint on a clock rather than by distance travelled
    // is an airbrush whatever it is called — that is the whole shape of one.
    if brush.is_timed() {
        return Style::AIRBRUSH;
    }
    // And a heavy smudge with a name that gives nothing away really is a
    // blender. See the module docs for why this is the last word and not the
    // first.
    if brush.smudge >= 0.5 {
        return Style::BLENDER;
    }
    Style::PAINT
}

/// Lowercase and drop everything that is not a letter or digit.
///
/// Pack names separate words with any of `_ - # . ( ) +` and space, sometimes
/// several in one name, so matching against the raw string would need every
/// rule spelled several ways.
fn fold(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Sort key for a category, for listing them in [`Style::ALL`] order.
///
/// Anything unrecognised — a collection from an imported library that names its
/// own — sorts after the built-in ones rather than being dropped or reordered
/// arbitrarily.
pub fn order_of(category: &str) -> usize {
    Style::ALL
        .iter()
        .position(|s| *s == category)
        .unwrap_or(Style::ALL.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str) -> &'static str {
        classify(name, &Brush::default())
    }

    #[test]
    fn separators_do_not_change_the_answer() {
        // The same brush arrives spelled four ways across the packs.
        assert_eq!(named("8B_Pencil#1"), Style::PENCIL);
        assert_eq!(named("pencil-8b"), Style::PENCIL);
        assert_eq!(named("subtle pencil"), Style::PENCIL);
        assert_eq!(named("2B_pencil"), Style::PENCIL);
    }

    #[test]
    fn erasing_settles_it_whatever_the_brush_is_called() {
        // The one setting that overrides the name. An eraser filed under paint
        // is actively misleading — you would reach for it to make a mark.
        let eraser = Brush {
            mode: BrushMode::Erase,
            ..Default::default()
        };
        assert_eq!(classify("wet_knife", &eraser), Style::ERASER);
        assert_eq!(classify("charcoal-04", &eraser), Style::ERASER);
        assert_eq!(classify("Airbrush_a", &eraser), Style::ERASER);
    }

    #[test]
    fn a_blender_made_of_something_is_filed_as_a_blender() {
        // These are the names the ordering exists for: each mentions a medium
        // *and* what is being done with it, and the doing is what matters.
        assert_eq!(named("charcoal-blur1"), Style::BLENDER);
        assert_eq!(named("smudge_ink(0.7)_sm"), Style::BLENDER);
        assert_eq!(named("blend+paint"), Style::BLENDER);
        assert_eq!(named("basic_digital_brush_smudging"), Style::BLENDER);
    }

    #[test]
    fn water_makes_a_paint_brush_wet_media() {
        assert_eq!(named("acrylic-03-with-water"), Style::WATERCOLOUR);
        assert_eq!(named("acrylic-03-paint"), Style::PAINT);
        assert_eq!(named("watercolor-02-paint"), Style::WATERCOLOUR);
    }

    #[test]
    fn a_timed_brush_with_no_telling_name_is_an_airbrush() {
        // The shape of an airbrush is that it keeps depositing while held
        // still. `Delayed_` and friends give the name no hint at all.
        let timed = Brush {
            dabs_per_second: 60.0,
            ..Default::default()
        };
        assert_eq!(classify("zzz_unknown", &timed), Style::AIRBRUSH);
        assert_eq!(classify("zzz_unknown", &Brush::default()), Style::PAINT);
    }

    #[test]
    fn every_rule_points_at_a_real_category() {
        // A typo in a rule's style would silently create a thirteenth
        // collection that the picker orders last and nobody expects.
        for (needle, style) in RULES {
            assert!(
                Style::ALL.contains(style),
                "rule {needle:?} names an unknown style {style:?}"
            );
        }
    }

    #[test]
    fn a_specific_name_beats_a_generic_one() {
        // `DNA_brush` contains "brush", and did land in Paint until the effect
        // rules were moved above the catch-alls. Same shape of mistake for the
        // rest of these.
        assert_eq!(named("DNA_brush"), Style::EFFECT);
        assert_eq!(named("rigger_brush"), Style::INK);
        assert_eq!(named("detail_brush_large"), Style::PAINT);
        assert_eq!(named("WateryFlatbrush"), Style::WATERCOLOUR);
        assert_eq!(named("wet_paint_sm"), Style::WATERCOLOUR);
        assert_eq!(named("Glow_Airbrush"), Style::AIRBRUSH);
    }

    #[test]
    fn a_painting_brush_that_mixes_is_still_a_painting_brush() {
        // Over a third of the MyPaint set smudges, oil paints included. Reading
        // that as "blender" collapsed 68 of 196 brushes into one collection and
        // left the paints nearly empty.
        let mixes = Brush {
            smudge: 0.8,
            ..Default::default()
        };
        assert_eq!(classify("oil-03-paint", &mixes), Style::PAINT);
        assert_eq!(classify("watercolor-02-paint", &mixes), Style::WATERCOLOUR);
        // Only when the name says blending is the point of it.
        assert_eq!(classify("blending_knife", &mixes), Style::BLENDER);
        // Or when the name says nothing at all.
        assert_eq!(classify("zzz_unknown", &mixes), Style::BLENDER);
    }

    #[test]
    fn categories_list_in_their_declared_order() {
        assert_eq!(order_of(Style::PENCIL), 0);
        assert!(order_of(Style::ERASER) < order_of(Style::EFFECT));
        assert_eq!(order_of("Something imported"), Style::ALL.len());
    }
}
