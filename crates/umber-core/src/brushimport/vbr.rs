//! GIMP `.vbr` parametric brushes.
//!
//! The one GIMP format Umber can reproduce **exactly** rather than approximate.
//! A `.vbr` is not a picture: it is a handful of numbers describing a
//! procedurally generated dab, and every one of them has a field on
//! [`Brush`] already. No tip is needed, no coverage is resampled, and the mark
//! Umber makes is the mark GIMP makes.
//!
//! Ten lines of text, or seven for the older revision:
//!
//! ```text
//! GIMP-VBR
//! 1.5            1.0 has no shape and no spike count
//! Chisel         name
//! square         shape      (1.5 only)
//! 10.0           spacing, per cent of the brush size
//! 20.0           radius, in pixels
//! 2              spikes     (1.5 only)
//! 0.5            hardness, 0..1
//! 4.0            aspect ratio, long axis over short
//! 30.0           angle, degrees
//! ```
//!
//! # What is dropped
//!
//! - **Square and diamond shapes.** Umber's dab is an ellipse, full stop —
//!   `dab.wgsl` tests `length(local) <= 1`. A square brush imported as a round
//!   one of the same radius is a different mark, so it is reported rather than
//!   passed off.
//! - **Spikes.** GIMP can generate a star of 3 to 20 points. Same reason.
//!
//! Both are stated in the file as plain words, so an import knows exactly when
//! it is approximating and can say which brush it happened to.

use crate::brush::Brush;
use crate::preset::PresetError;

/// Revisions this understands. `1.0` has no shape and no spike count; `1.5`
/// added both and is what every GIMP since 2.4 writes.
const VERSIONS: [&str; 2] = ["1.0", "1.5"];

/// A decoded parametric brush.
#[derive(Clone, Debug)]
pub struct VbrBrush {
    pub name: String,
    pub brush: Brush,
    /// The shape word from the file, for a reader that wants to say which one
    /// it could not draw. `None` for a circle, which Umber draws exactly.
    pub unsupported_shape: Option<&'static str>,
    /// True when the file asked for a star rather than a blob.
    pub spiked: bool,
}

/// Decode a GIMP `.vbr` file.
pub fn from_vbr(text: &str) -> Result<VbrBrush, PresetError> {
    // Trailing blank lines are common and `lines()` would hand them over as
    // fields; taking the non-empty ones in order is what a text format of
    // positional lines actually means.
    let mut lines = text.lines().map(str::trim);

    match lines.next() {
        Some(magic) if magic.starts_with("GIMP-VBR") => {}
        _ => return Err(malformed("it does not start with GIMP-VBR")),
    }

    let version = lines
        .next()
        .ok_or_else(|| malformed("it ends before its version"))?;
    // `starts_with`, as GIMP does: some writers append a build number.
    let Some(version) = VERSIONS.iter().find(|v| version.starts_with(**v)) else {
        return Err(malformed("its version is not 1.0 or 1.5"));
    };
    let extended = *version == "1.5";

    let name = lines
        .next()
        .ok_or_else(|| malformed("it ends before its name"))?
        .to_string();

    // GIMP's own shape names, and the two Umber cannot draw.
    let unsupported_shape = if extended {
        match lines.next() {
            Some("circle") => None,
            Some("square") => Some("square brush shapes"),
            Some("diamond") => Some("diamond brush shapes"),
            Some(other) => {
                return Err(PresetError::Malformed(
                    None,
                    format!("`{other}` is not a shape GIMP writes"),
                ));
            }
            None => return Err(malformed("it ends before its shape")),
        }
    } else {
        None
    };

    let spacing = number(&mut lines, "spacing")?;
    let radius = number(&mut lines, "radius")?;
    // GIMP writes 2 for "not a star" and 3..20 for one.
    let spikes = if extended {
        number(&mut lines, "spike count")?.round()
    } else {
        2.0
    };
    let hardness = number(&mut lines, "hardness")?;
    let aspect_ratio = number(&mut lines, "aspect ratio")?;
    let angle = number(&mut lines, "angle")?;

    let default = Brush::default();
    let brush = Brush {
        // GIMP states a radius; `Brush::size` is the diameter of the long axis,
        // and GIMP's radius *is* the long axis — the short one is
        // `radius / aspect_ratio`. So the two agree without a correction.
        size: (radius * 2.0).clamp(Brush::MIN_SIZE, Brush::MAX_SIZE),
        hardness: hardness.clamp(0.0, 1.0),
        // Per cent of the brush size, exactly as `.gbr` states it, and zero
        // means the same thing there: a writer that never filled the field in.
        spacing: if spacing > 0.0 {
            (spacing / 100.0).clamp(0.01, 4.0)
        } else {
            default.spacing
        },
        dab_ratio: aspect_ratio.clamp(1.0, 20.0),
        dab_angle: angle.rem_euclid(360.0),
        // A parametric brush carries no dynamics: GIMP stamps one at a constant
        // size unless a separate dynamics preset says otherwise, exactly as
        // with a `.gbr`. Leaving Umber's pressure-to-size mapping on would
        // shrink it to 8 % of itself at the start of every line.
        pressure_size: false,
        pressure_opacity: false,
        ..default
    };

    Ok(VbrBrush {
        name,
        brush,
        unsupported_shape,
        spiked: spikes != 2.0,
    })
}

/// What reading this `.vbr` will throw away.
pub fn dropped_features(text: &str) -> Vec<&'static str> {
    let Ok(decoded) = from_vbr(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    out.extend(decoded.unsupported_shape);
    if decoded.spiked {
        out.push("star-shaped brushes");
    }
    out
}

fn number<'a>(lines: &mut impl Iterator<Item = &'a str>, field: &str) -> Result<f32, PresetError> {
    let line = lines
        .next()
        .ok_or_else(|| PresetError::Malformed(None, format!("it ends before its {field}")))?;
    line.parse()
        .map_err(|_| PresetError::Malformed(None, format!("its {field} is `{line}`, not a number")))
}

fn malformed(message: &str) -> PresetError {
    PresetError::Malformed(None, message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHISEL: &str = "GIMP-VBR\n1.5\nChisel\ncircle\n25.0\n20.0\n2\n0.75\n4.0\n30.0\n";

    #[test]
    fn a_parametric_brush_lands_on_umbers_own_dab() {
        // The whole reason this format is worth reading: nothing here is
        // approximated, so the assertions can be exact.
        let decoded = from_vbr(CHISEL).expect("decode");
        assert_eq!(decoded.name, "Chisel");
        assert_eq!(decoded.brush.size, 40.0);
        assert_eq!(decoded.brush.spacing, 0.25);
        assert_eq!(decoded.brush.hardness, 0.75);
        assert_eq!(decoded.brush.dab_ratio, 4.0);
        assert_eq!(decoded.brush.dab_angle, 30.0);
        // No dynamics in the file, so none invented. GIMP stamps a `.vbr` at a
        // constant size.
        assert!(!decoded.brush.pressure_size);
        assert!(decoded.unsupported_shape.is_none());
        assert!(dropped_features(CHISEL).is_empty());
    }

    /// 1.0 has no shape line and no spike line, so reading it as 1.5 would
    /// take the spacing for a shape and shift every field after it.
    #[test]
    fn the_older_revision_has_two_fewer_fields() {
        let old = "GIMP-VBR\n1.0\nRound\n10.0\n6.0\n0.5\n1.0\n0.0\n";
        let decoded = from_vbr(old).expect("decode");
        assert_eq!(decoded.name, "Round");
        assert_eq!(decoded.brush.size, 12.0);
        assert_eq!(decoded.brush.spacing, 0.1);
        assert_eq!(decoded.brush.hardness, 0.5);
        assert_eq!(decoded.brush.dab_ratio, 1.0);
    }

    /// Umber's dab is an ellipse and `dab.wgsl` says so with
    /// `length(local) <= 1`. A square imported as a circle is a different mark,
    /// and the file names the shape, so there is no excuse for not saying it.
    #[test]
    fn a_shape_umber_cannot_draw_is_named_rather_than_rounded_off() {
        let square = "GIMP-VBR\n1.5\nBlock\nsquare\n10.0\n8.0\n2\n1.0\n1.0\n0.0\n";
        assert_eq!(dropped_features(square), ["square brush shapes"]);
        let diamond = "GIMP-VBR\n1.5\nGem\ndiamond\n10.0\n8.0\n2\n1.0\n1.0\n0.0\n";
        assert_eq!(dropped_features(diamond), ["diamond brush shapes"]);

        // The brush still arrives — an approximation of a brush you chose beats
        // a refusal — it just does not pretend to be square.
        assert_eq!(from_vbr(square).expect("decode").brush.size, 16.0);
    }

    #[test]
    fn a_star_is_reported_too() {
        let star = "GIMP-VBR\n1.5\nStar\ncircle\n10.0\n8.0\n5\n1.0\n1.0\n0.0\n";
        assert_eq!(dropped_features(star), ["star-shaped brushes"]);
        // Both at once read as two losses, not one.
        let both = "GIMP-VBR\n1.5\nStar\nsquare\n10.0\n8.0\n5\n1.0\n1.0\n0.0\n";
        assert_eq!(
            dropped_features(both),
            ["square brush shapes", "star-shaped brushes"]
        );
    }

    #[test]
    fn a_spacing_of_zero_means_unset_rather_than_none_at_all() {
        let odd = "GIMP-VBR\n1.0\nR\n0.0\n8.0\n1.0\n1.0\n0.0\n";
        assert_eq!(
            from_vbr(odd).unwrap().brush.spacing,
            Brush::default().spacing
        );
    }

    #[test]
    fn nonsense_is_an_error_rather_than_a_panic() {
        assert!(from_vbr("").is_err());
        assert!(from_vbr("not a brush").is_err());
        assert!(from_vbr("GIMP-VBR\n9.9\nX\n").is_err());
        assert!(from_vbr("GIMP-VBR\n1.5\nX\ncircle\n").is_err());
        assert!(from_vbr("GIMP-VBR\n1.5\nX\nhexagon\n1\n1\n2\n1\n1\n0\n").is_err());
        assert!(from_vbr("GIMP-VBR\n1.0\nX\nnope\n8\n1\n1\n0\n").is_err());
        assert!(dropped_features("rubbish").is_empty());

        for cut in 0..CHISEL.len() {
            let _ = from_vbr(&CHISEL[..cut]);
        }
    }
}
