//! Turning imported pixels into the exact bytes a layer texture holds.
//!
//! Every format this module reads — ORA, KRA, PSD, PNG — stores **sRGB-encoded,
//! straight-alpha** RGBA8. Umber's layer textures do not hold that. They are
//! `Rgba8UnormSrgb`, and `commit.wgsl` renders *premultiplied linear* colour
//! into them, so the sampler in `composite.wgsl` can treat what it reads as
//! premultiplied (`composite_over` divides by `src.a`). The byte actually
//! stored is therefore
//!
//! ```text
//!     stored = srgb_encode( srgb_decode(source) * alpha )
//! ```
//!
//! Getting this wrong is the classic import bug and it is nearly invisible on
//! test images: opaque pixels are a no-op under this transform (`alpha = 1`),
//! so a wrong conversion looks perfect on every screenshot without transparency
//! and only shows up as haloed or washed-out edges on a real drawing. That is
//! why `opaque_pixels_pass_through_unchanged` is not the only test here.
//!
//! Note the premultiply happens in **linear** space and the encode after it.
//! Multiplying the sRGB byte by alpha instead — the tempting one-liner — is
//! wrong by a full gamma curve on every partly transparent pixel.

use std::sync::OnceLock;

use crate::color::{linear_to_srgb, srgb_to_linear};

/// `[alpha][value] -> stored byte`. 64 KiB, built once.
///
/// A table rather than two `powf` calls per component: a 4096² import is 16
/// million pixels, and `powf` on 48 million components takes seconds. Every
/// input is one of 65 536 (value, alpha) pairs, so the table is exact rather
/// than an approximation.
static TABLE: OnceLock<Box<[u8; 256 * 256]>> = OnceLock::new();

fn table() -> &'static [u8; 256 * 256] {
    TABLE.get_or_init(|| {
        let mut t = Box::new([0u8; 256 * 256]);
        for a in 0..256usize {
            let alpha = a as f32 / 255.0;
            for v in 0..256usize {
                let linear = srgb_to_linear(v as f32 / 255.0);
                let encoded = linear_to_srgb(linear * alpha);
                t[a * 256 + v] = (encoded * 255.0 + 0.5) as u8;
            }
        }
        t
    })
}

/// Convert one straight-alpha sRGB pixel into the layer-texture form.
pub fn encode_pixel(px: [u8; 4]) -> [u8; 4] {
    let t = table();
    let a = px[3] as usize * 256;
    [
        t[a + px[0] as usize],
        t[a + px[1] as usize],
        t[a + px[2] as usize],
        px[3],
    ]
}

/// Convert a whole RGBA8 buffer in place.
///
/// # Panics
///
/// Debug-asserts that `buf` is a whole number of pixels; a partial pixel means
/// a reader miscounted, which is a bug worth catching in tests.
pub fn encode_buffer(buf: &mut [u8]) {
    debug_assert_eq!(buf.len() % 4, 0, "not a whole number of RGBA pixels");
    for px in buf.chunks_exact_mut(4) {
        px.copy_from_slice(&encode_pixel([px[0], px[1], px[2], px[3]]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Color;

    #[test]
    fn opaque_pixels_pass_through_unchanged() {
        // alpha = 1 makes the premultiply the identity, and encode(decode(v))
        // must round-trip exactly or every opaque import shifts colour.
        for v in 0..=255u8 {
            let out = encode_pixel([v, v, v, 255]);
            assert_eq!(out, [v, v, v, 255], "byte {v} did not survive");
        }
    }

    #[test]
    fn fully_transparent_pixels_lose_their_colour() {
        // Premultiplied storage has no way to keep colour at zero alpha, and
        // the composite shader would divide by zero if it tried.
        assert_eq!(encode_pixel([255, 40, 10, 0]), [0, 0, 0, 0]);
    }

    #[test]
    fn half_alpha_white_is_linear_not_srgb_half() {
        // The whole point. White at 50% alpha stores linear 0.5, which encodes
        // to sRGB ~188 — not 128, which is what multiplying the sRGB byte by
        // alpha would give. 128 here means the premultiply moved into the wrong
        // space.
        let out = encode_pixel([255, 255, 255, 128]);
        assert!(
            (out[0] as i32 - 188).abs() <= 1,
            "expected ~188, got {}",
            out[0]
        );
        assert_eq!(out[3], 128, "alpha must not be touched");
    }

    #[test]
    fn the_encode_matches_the_engines_own_colour_maths() {
        // Independent derivation of the same value through `Color`, so the
        // table cannot drift away from the rest of the engine.
        for (v, a) in [(200u8, 90u8), (17, 200), (255, 1), (64, 128)] {
            let linear = Color::from_srgb_u8(v, v, v, a);
            let expected = (linear_to_srgb(linear.r * linear.a) * 255.0 + 0.5) as u8;
            assert_eq!(encode_pixel([v, v, v, a])[0], expected, "v={v} a={a}");
        }
    }

    #[test]
    fn buffer_and_pixel_paths_agree() {
        let mut buf = vec![10, 20, 30, 40, 255, 128, 0, 200];
        encode_buffer(&mut buf);
        assert_eq!(&buf[0..4], encode_pixel([10, 20, 30, 40]));
        assert_eq!(&buf[4..8], encode_pixel([255, 128, 0, 200]));
    }
}
