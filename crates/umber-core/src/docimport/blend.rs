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

        // `plus` is Porter-Duff addition on premultiplied colour; Umber's Add
        // clamps the sum of straight colour. The two agree wherever both
        // layers are opaque, which is most of the time, and differ at soft
        // edges — so: approximate, not exact.
        "plus" | "linear-dodge" | "add" => (BlendMode::Add, Approximate),

        // Same family, different curve.
        "darken" | "color-burn" | "linear-burn" => (BlendMode::Multiply, Approximate),
        "lighten" | "color-dodge" => (BlendMode::Screen, Approximate),
        "hard-light" | "soft-light" | "vivid-light" | "linear-light" | "pin-light" => {
            (BlendMode::Overlay, Approximate)
        }

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

    #[test]
    fn every_umber_mode_is_reachable() {
        // If a mode has no source name mapping to it, an import can never
        // produce it — which would mean the table has a hole.
        for mode in BlendMode::ALL {
            assert!(
                [
                    "src-over",
                    "multiply",
                    "screen",
                    "overlay",
                    "plus",
                    "darken",
                    "hard-light"
                ]
                .iter()
                .any(|n| nearest(n).0 == mode),
                "{:?} unreachable",
                mode.label()
            );
        }
    }

    #[test]
    fn unknown_modes_fall_back_loudly() {
        for name in ["difference", "hue", "luminosity", "dst-out", "invented"] {
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
