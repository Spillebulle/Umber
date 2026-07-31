//! Colour handling.
//!
//! Everything inside the engine is **linear** RGBA. Blending in sRGB space is
//! subtly wrong (it darkens midtones), so conversion happens only at the
//! boundaries: UI colour pickers hand us sRGB, and the surface is configured
//! with an sRGB format so the hardware encodes on write.

/// Linear-space RGBA colour with components in `0.0..=1.0`, straight (not
/// premultiplied) alpha.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const TRANSPARENT: Self = Self::new(0.0, 0.0, 0.0, 0.0);
    pub const BLACK: Self = Self::new(0.0, 0.0, 0.0, 1.0);
    pub const WHITE: Self = Self::new(1.0, 1.0, 1.0, 1.0);

    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    /// Build from sRGB-encoded bytes, the form colour pickers and hex codes use.
    pub fn from_srgb_u8(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self {
            r: srgb_to_linear(r as f32 / 255.0),
            g: srgb_to_linear(g as f32 / 255.0),
            b: srgb_to_linear(b as f32 / 255.0),
            a: a as f32 / 255.0,
        }
    }

    /// Back to sRGB-encoded bytes for display in the UI.
    pub fn to_srgb_u8(self) -> [u8; 4] {
        [
            (linear_to_srgb(self.r) * 255.0 + 0.5) as u8,
            (linear_to_srgb(self.g) * 255.0 + 0.5) as u8,
            (linear_to_srgb(self.b) * 255.0 + 0.5) as u8,
            (self.a * 255.0 + 0.5) as u8,
        ]
    }

    /// Linear components as a GPU-ready array.
    pub fn to_array(self) -> [f32; 4] {
        [self.r, self.g, self.b, self.a]
    }

    pub fn with_alpha(self, a: f32) -> Self {
        Self { a, ..self }
    }
}

/// Hue/saturation/value, the space colour pickers are built on.
///
/// Deliberately defined over **sRGB** components rather than linear ones: a
/// picker is a perceptual instrument, and running HSV over linear values makes
/// the saturation/value square look badly bunched towards the dark end.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hsv {
    /// Degrees, `0.0..360.0`.
    pub h: f32,
    /// `0.0..=1.0`.
    pub s: f32,
    /// `0.0..=1.0`.
    pub v: f32,
}

impl Hsv {
    pub fn new(h: f32, s: f32, v: f32) -> Self {
        Self {
            h: h.rem_euclid(360.0),
            s: s.clamp(0.0, 1.0),
            v: v.clamp(0.0, 1.0),
        }
    }

    /// Convert to a linear-space colour.
    pub fn to_color(self, alpha: f32) -> Color {
        let h = self.h.rem_euclid(360.0) / 60.0;
        let c = self.v * self.s;
        let x = c * (1.0 - (h % 2.0 - 1.0).abs());
        let m = self.v - c;
        let (r, g, b) = match h as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        Color {
            r: srgb_to_linear(r + m),
            g: srgb_to_linear(g + m),
            b: srgb_to_linear(b + m),
            a: alpha,
        }
    }
}

impl Color {
    /// Decompose into HSV over sRGB components.
    ///
    /// Hue is undefined for greys and comes back as 0; a picker should keep its
    /// own hue rather than round-tripping through here, or dragging value to
    /// zero would silently reset the hue to red.
    pub fn to_hsv(self) -> Hsv {
        let r = linear_to_srgb(self.r);
        let g = linear_to_srgb(self.g);
        let b = linear_to_srgb(self.b);

        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let d = max - min;

        let h = if d <= f32::EPSILON {
            0.0
        } else if max == r {
            60.0 * (((g - b) / d) % 6.0)
        } else if max == g {
            60.0 * ((b - r) / d + 2.0)
        } else {
            60.0 * ((r - g) / d + 4.0)
        };

        Hsv {
            h: h.rem_euclid(360.0),
            s: if max <= f32::EPSILON { 0.0 } else { d / max },
            v: max,
        }
    }
}

pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.040_448_237 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

pub fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_roundtrip_is_stable() {
        for byte in 0..=255u8 {
            let c = Color::from_srgb_u8(byte, byte, byte, 255);
            assert_eq!(c.to_srgb_u8()[0], byte, "byte {byte} did not round-trip");
        }
    }

    #[test]
    fn hsv_round_trips_through_colour() {
        for (h, s, v) in [
            (0.0, 1.0, 1.0),
            (120.0, 0.5, 0.8),
            (240.0, 1.0, 0.35),
            (37.0, 0.62, 0.75),
            (300.0, 0.2, 1.0),
        ] {
            let hsv = Hsv::new(h, s, v);
            let back = hsv.to_color(1.0).to_hsv();
            assert!((back.h - h).abs() < 0.5, "hue {h} -> {}", back.h);
            assert!((back.s - s).abs() < 0.01, "sat {s} -> {}", back.s);
            assert!((back.v - v).abs() < 0.01, "val {v} -> {}", back.v);
        }
    }

    #[test]
    fn hsv_primaries_land_on_the_right_bytes() {
        assert_eq!(
            Hsv::new(0.0, 1.0, 1.0).to_color(1.0).to_srgb_u8(),
            [255, 0, 0, 255]
        );
        assert_eq!(
            Hsv::new(120.0, 1.0, 1.0).to_color(1.0).to_srgb_u8(),
            [0, 255, 0, 255]
        );
        assert_eq!(
            Hsv::new(240.0, 1.0, 1.0).to_color(1.0).to_srgb_u8(),
            [0, 0, 255, 255]
        );
    }

    #[test]
    fn hsv_is_defined_over_srgb_not_linear() {
        // 50% sRGB grey must report value 0.5, not the 0.21 that running HSV
        // over linear components would give.
        let v = Color::from_srgb_u8(128, 128, 128, 255).to_hsv().v;
        assert!((v - 0.502).abs() < 0.01, "got {v}");
    }

    #[test]
    fn greys_have_zero_saturation() {
        let hsv = Color::from_srgb_u8(90, 90, 90, 255).to_hsv();
        assert!(hsv.s < 1e-4, "got {}", hsv.s);
    }

    #[test]
    fn linear_midpoint_is_darker_than_srgb_midpoint() {
        // 50% sRGB grey is ~21% linear. Getting this backwards is the classic
        // washed-out-blending bug, so pin it down.
        let mid = Color::from_srgb_u8(128, 128, 128, 255);
        assert!(mid.r > 0.20 && mid.r < 0.23, "got {}", mid.r);
    }
}
