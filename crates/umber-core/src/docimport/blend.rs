//! Mapping other applications' blend modes onto Umber's five.
//!
//! Umber has Normal, Multiply, Screen, Overlay and Add. Photoshop has
//! twenty-seven, Krita has rather more than that. Every import therefore has to
//! choose a nearest mode, and the one thing it must not do is choose silently:
//! a Difference layer arriving as Normal changes the picture completely, and a
//! user who was not told will conclude the importer is broken rather than
//! incomplete.
//!
//! So the policy lives here, in one table, and returns a [`Fidelity`] alongside
//! the mode. Each format module normalises its own vocabulary to the canonical
//! names below rather than repeating the policy.

use crate::layer::BlendMode;

/// How well the chosen [`BlendMode`] represents the source mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fidelity {
    /// Umber implements the same formula. Nothing was lost.
    Exact,
    /// A related mode that moves the image in the same direction — Darken
    /// arriving as Multiply, say. Visibly different, but recognisably the
    /// artist's intent.
    Approximate,
    /// Umber has nothing like it; the layer composites as Normal.
    Dropped,
}

/// Nearest Umber mode for a canonical blend-mode name.
///
/// Names are the OpenRaster/SVG spelling with the `svg:` prefix removed, since
/// that vocabulary is a published standard and the other two formats map onto
/// it cleanly. Unknown names are Normal/`Dropped` rather than an error: a file
/// that uses a mode we have never heard of should still open.
pub fn nearest(canonical: &str) -> (BlendMode, Fidelity) {
    use Fidelity::{Approximate, Dropped, Exact};
    match canonical {
        // Exact: Umber's composite.wgsl implements the same W3C formula.
        "src-over" | "normal" => (BlendMode::Normal, Exact),
        "multiply" => (BlendMode::Multiply, Exact),
        "screen" => (BlendMode::Screen, Exact),
        "overlay" => (BlendMode::Overlay, Exact),
        "darken" => (BlendMode::Darken, Exact),
        "lighten" => (BlendMode::Lighten, Exact),
        "color-dodge" => (BlendMode::ColorDodge, Exact),
        "color-burn" => (BlendMode::ColorBurn, Exact),
        "hard-light" => (BlendMode::HardLight, Exact),
        "soft-light" => (BlendMode::SoftLight, Exact),
        "difference" => (BlendMode::Difference, Exact),
        "exclusion" => (BlendMode::Exclusion, Exact),
        "hue" => (BlendMode::Hue, Exact),
        "saturation" => (BlendMode::Saturation, Exact),
        "color" => (BlendMode::Color, Exact),
        "luminosity" => (BlendMode::Luminosity, Exact),

        // Photoshop's own, which SVG has no name for and Umber now implements
        // from the same definitions Photoshop and Clip Studio use.
        "linear-burn" => (BlendMode::LinearBurn, Exact),
        "vivid-light" => (BlendMode::VividLight, Exact),
        "linear-light" => (BlendMode::LinearLight, Exact),
        "pin-light" => (BlendMode::PinLight, Exact),
        "subtract" => (BlendMode::Subtract, Exact),
        "divide" => (BlendMode::Divide, Exact),

        // **`linear-dodge` *is* Add**, which is why it is exact and `plus` is
        // not. Photoshop and Clip Studio both spell the mode "Linear Dodge
        // (Add)" and it is `min(Cb + Cs, 1)`, the formula `blend_rgb` has.
        // They shared an arm, so eight of the thirty-three documents this was
        // measured against raised a `BlendApproximated` warning about a layer
        // that had arrived perfectly.
        "linear-dodge" | "add" => (BlendMode::Add, Exact),

        // `plus` is Porter-Duff addition on premultiplied colour; Umber's Add
        // clamps the sum of straight colour. The two agree wherever both
        // layers are opaque, which is most of the time, and differ at soft
        // edges — so: approximate, not exact.
        // `add-glow` is Clip Studio's Add that ignores what is under it where
        // that is transparent — the same direction, one step further from the
        // formula Umber has.
        "plus" | "add-glow" => (BlendMode::Add, Approximate),

        // `glow-dodge` is Clip Studio's dodge that keeps highlights, which is
        // Colour Dodge treating a transparent backdrop differently.
        "glow-dodge" => (BlendMode::ColorDodge, Approximate),

        // The two "colour" comparisons pick a whole pixel by luminosity rather
        // than working per channel, which nothing here does. Lighten and Darken
        // move the image the same way and are the closest thing Umber has.
        "darker-color" => (BlendMode::Darken, Approximate),
        "lighter-color" => (BlendMode::Lighten, Approximate),

        // Hard Mix posterises to the eight corners of the colour cube. Vivid
        // Light is the curve it is the limit of, so it is the same family —
        // but the result is so much flatter that this stays approximate.
        "hard-mix" => (BlendMode::VividLight, Approximate),

        // Nothing in Umber moves the image the way these do, and picking a
        // "close" mode would be a worse lie than Normal.
        _ => (BlendMode::Normal, Dropped),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_shared_modes_are_exact() {
        for (name, mode) in [
            ("src-over", BlendMode::Normal),
            ("multiply", BlendMode::Multiply),
            ("screen", BlendMode::Screen),
            ("overlay", BlendMode::Overlay),
        ] {
            assert_eq!(nearest(name), (mode, Fidelity::Exact), "{name}");
        }
    }

    /// Every mode Umber has can be *arrived at* by an import.
    ///
    /// A mode with no source name mapping to it is a hole in the table: the
    /// engine can draw it and the interface can set it, but no document will
    /// ever open as it. The names are the canonical (SVG, `svg:` stripped)
    /// spellings plus the two Photoshop families OpenRaster never named.
    ///
    /// **Driven off `BlendMode::ALL`**, so a mode added to the enum fails here
    /// until `nearest` can produce it — which is what caught Colour Burn when
    /// the set grew, since its label is British and its canonical name is not.
    #[test]
    fn every_umber_mode_is_reachable() {
        const NAMES: [&str; 23] = [
            "src-over",
            "multiply",
            "screen",
            "overlay",
            "linear-dodge",
            "darken",
            "lighten",
            "color-dodge",
            "color-burn",
            "linear-burn",
            "hard-light",
            "soft-light",
            "vivid-light",
            "linear-light",
            "pin-light",
            "difference",
            "exclusion",
            "subtract",
            "divide",
            "hue",
            "saturation",
            "color",
            "luminosity",
        ];
        for mode in BlendMode::ALL {
            assert!(
                NAMES.iter().any(|n| nearest(n).0 == mode),
                "{:?} unreachable",
                mode.label()
            );
        }
    }

    /// A name nothing here has heard of composites as Normal and says so.
    ///
    /// The examples used to be `difference`, `hue` and `luminosity`, which is
    /// exactly the shape that goes stale: every one of them is now a mode Umber
    /// implements. What is left has to be things OpenRaster genuinely does not
    /// define as a *blend* — the Porter-Duff compositing operators, which move
    /// alpha rather than colour and are a different question — and outright
    /// invention.
    #[test]
    fn unknown_modes_fall_back_loudly() {
        for name in ["dst-out", "dst-in", "src-atop", "xor", "invented"] {
            assert_eq!(
                nearest(name),
                (BlendMode::Normal, Fidelity::Dropped),
                "{name}"
            );
        }
    }

    #[test]
    fn plain_normal_is_never_reported_as_a_loss() {
        // The common case must not spray warnings: almost every layer in a
        // real document is Normal, and a warning list that cries wolf is one
        // nobody reads.
        assert_eq!(nearest("normal").1, Fidelity::Exact);
        assert_eq!(nearest("src-over").1, Fidelity::Exact);
    }
}
