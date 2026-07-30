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
    fn linear_midpoint_is_darker_than_srgb_midpoint() {
        // 50% sRGB grey is ~21% linear. Getting this backwards is the classic
        // washed-out-blending bug, so pin it down.
        let mid = Color::from_srgb_u8(128, 128, 128, 255);
        assert!(mid.r > 0.20 && mid.r < 0.23, "got {}", mid.r);
    }
}
