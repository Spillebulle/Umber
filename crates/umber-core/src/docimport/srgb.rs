//! Turning imported pixels into the exact bytes a layer texture holds, and
//! back again.
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
//!
//! # The other direction
//!
//! [`decode_pixel`] inverts all of that, because saving a document has to write
//! the straight-alpha sRGB that every interchange format — ORA included —
//! stores. The two must remain exact inverses on the bytes a layer texture can
//! actually hold, or a document would drift a little every time it was saved and
//! reopened; `saving_and_reopening_does_not_move_a_pixel` pins that down.
//!
//! # A mask is *not* the same question, and this file used to answer it wrongly
//!
//! A mask is another slice of the same layer array, so for a long time it was
//! read through the same `Rgba8UnormSrgb` view a layer's colour is read
//! through — which meant the stored byte was **sRGB-encoded coverage**, and
//! every source format's linear multiplier had to be encoded on the way in.
//! This module carried a `coverage_table` doing exactly that.
//!
//! That was inherited rather than chosen, and it cost precision. A mask is a
//! multiplier on **alpha**, and alpha is not gamma-encoded anywhere in Umber:
//! `LAYER_FORMAT`'s own docs say an sRGB format encodes RGB only, and
//! `STROKE_FORMAT` is justified as being exactly as wide as the linear alpha it
//! lands in. Squeezing coverage through the transfer function is therefore a
//! conversion into a space nothing downstream wanted, and the map is **not
//! injective**: `round(linear_to_srgb(v/255)·255)` has a slope below one from
//! input 75 upward, so 73 of 256 inputs collide with their neighbour. Measured
//! both ways and it is the same figure from either end — an sRGB-stored mask can
//! express only **183** of the 256 multipliers the composite's own 8-bit alpha
//! can show. Adjacent stored bytes differ by 0.0089 in the multiplier at the
//! reveal end, against a uniform 0.0039 for linear.
//!
//! **It is a trade and not a free win, and saying so is the honest half.** A
//! mask scales premultiplied RGBA, and the two halves land in channels of
//! different kinds: alpha is linear 8-bit, colour is sRGB 8-bit. The counts are
//! exactly mirrored — linear storage reaches 256 alphas and 183 colours, sRGB
//! storage 183 and 256 — and they have to be, because the two storages differ by
//! the transfer function and so do the two destinations. What decides it is that
//! a mask is a multiplier on **alpha**: that is the channel it scales, it is
//! what a transparent-background document exports, and it is the form every
//! source format already states. What it costs is the hide end over an *opaque*
//! backdrop, where the first non-zero mask level now takes the output from
//! sRGB 0 to sRGB 13 where it used to step 0, 1, 2 — so a light layer masked
//! down over dark artwork bands there where it did not before.
//! `a_mask_multiplier_reaches_every_level_the_composite_can_show` measures all
//! four cells, because a guard that took one column would have read as proof of
//! something that is only half true.
//!
//! So a mask slice now holds **linear coverage**, read through the array's
//! `LAYER_FORMAT_LINEAR` view — the same raw view `flip.wgsl` has always used,
//! and for the same reason: these bytes are not colour and must not go through a
//! transfer function. `byte / 255` *is* the multiplier, all 256 of them, and a
//! source format's byte is now copied across unchanged because it already means
//! what this one means. [`mask_pixel`] is the whole of the conversion, and it is
//! a widen rather than a conversion.
//!
//! [`decode_v3_mask_buffer`] is what remains of the old map: a document written
//! before this change holds the sRGB form, and its bytes are converted on the
//! way in. **That cannot be a lossless migration and is not claimed to be** —
//! the old encode collapsed 73 of its inputs, so the state that produced a given
//! stored byte is not recoverable. What is recoverable is the *multiplier*,
//! which is the whole of what a mask does, and requantising it onto the linear
//! grid moves it by at most 0.499 of one level of the layer's own 8-bit alpha —
//! under the rounding the composite's multiply already performs. So an old
//! document opens looking as it did, and a version-3 file taken to 4 and back
//! would not come back byte for byte. No route asks it to.

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
    for px in buf.as_chunks_mut::<4>().0.iter_mut() {
        *px = encode_pixel(*px);
    }
}

/// Widen one byte of a mask into the four a mask slice holds.
///
/// The input is coverage as every source format states it and as a mask slice
/// now holds it — a **linear** multiplier on the layer's alpha, `0` hiding and
/// `255` revealing. There is no conversion: the composite reads a mask through
/// the layer array's `LAYER_FORMAT_LINEAR` view, so the stored byte over 255 is
/// the multiplier, and the source already means that. See the module docs for
/// what this used to do and what it cost.
///
/// Opaque in the fourth byte because a mask slice is read on one channel and
/// nothing looks at the others; writing the coverage there as well would make
/// a half-hidden layer's mask *itself* half transparent the day something does.
pub fn mask_pixel(coverage: u8) -> [u8; 4] {
    [coverage, coverage, coverage, 255]
}

/// Widen a canvas of coverage bytes into a mask slice.
pub fn mask_buffer(coverage: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(coverage.len() * 4);
    for &c in coverage {
        out.extend_from_slice(&mask_pixel(c));
    }
    out
}

/// `stored byte -> the byte a mask slice holds`, for a document written before
/// [`crate::docformat::VERSION`] 4.
///
/// Those files hold sRGB-encoded coverage, so the multiplier they meant is
/// `srgb_to_linear(s / 255)` and the byte that means the same thing now is that
/// figure back in eight bits.
///
/// **This cannot be a lossless migration and must not be described as one.**
/// The old encode collapsed 73 of its 256 inputs onto a neighbour, so the state
/// that produced a given stored byte is not recoverable — only the multiplier
/// is, and that is what matters, because the multiplier is the whole of what a
/// mask does. What the requantisation costs is bounded and was measured: the
/// worst multiplier shift over all 256 stored bytes is 0.499 of one level of the
/// layer's own 8-bit alpha, which is under the rounding the composite already
/// performs when it multiplies. So a version-3 document's picture cannot move by
/// a level. A version-3 file converted to 4 and then read by a version-3 build
/// would not come back byte for byte, and there is no route that asks it to.
///
/// It takes the widened `(g, g, g, 255)` form both the reader and the saved
/// history produce, and rewrites all three colour bytes, because nothing
/// downstream promises which channel it reads: `composite.wgsl` takes `.r` and
/// `docformat`'s writer takes `px[0]`, and a slice whose channels disagreed
/// would be a trap for whichever is asked next.
///
/// **One function rather than a scalar with a buffer wrapper**, so there is no
/// second statement of the map to drift, and so the table is fetched once per
/// call rather than three times per pixel — a canvas-sized mask is millions of
/// them. A table for the reason [`TABLE`] is one: this is a `powf` per byte
/// otherwise.
static V3_COVERAGE: OnceLock<[u8; 256]> = OnceLock::new();

fn v3_coverage_table() -> &'static [u8; 256] {
    V3_COVERAGE.get_or_init(|| {
        let mut t = [0u8; 256];
        for (s, out) in t.iter_mut().enumerate() {
            *out = (srgb_to_linear(s as f32 / 255.0) * 255.0 + 0.5) as u8;
        }
        t
    })
}

/// A whole pre-version-4 mask slice, converted in place. See [`V3_COVERAGE`].
pub fn decode_v3_mask_buffer(buf: &mut [u8]) {
    debug_assert_eq!(buf.len() % 4, 0, "not a whole number of RGBA pixels");
    let t = v3_coverage_table();
    for px in buf.as_chunks_mut::<4>().0.iter_mut() {
        let c = t[px[0] as usize];
        px[0] = c;
        px[1] = c;
        px[2] = c;
    }
}

/// `[alpha][stored] -> straight byte`. The inverse of [`TABLE`].
static UNTABLE: OnceLock<Box<[u8; 256 * 256]>> = OnceLock::new();

fn untable() -> &'static [u8; 256 * 256] {
    UNTABLE.get_or_init(|| {
        let mut t = Box::new([0u8; 256 * 256]);
        for a in 1..256usize {
            let alpha = a as f32 / 255.0;
            for s in 0..256usize {
                let linear = srgb_to_linear(s as f32 / 255.0);
                // A stored value above its own alpha cannot come from a real
                // layer — premultiplied colour is bounded by it — but a file
                // damaged in transit could produce one, and `linear_to_srgb`
                // of something over 1.0 is not a colour.
                let straight = (linear / alpha).min(1.0);
                t[a * 256 + s] = (linear_to_srgb(straight) * 255.0 + 0.5) as u8;
            }
        }
        // Alpha 0 stays at the zeroed row: there is no colour to recover, and
        // inventing one would put ink into pixels the artist erased.
        t
    })
}

/// Convert one layer-texture pixel back to straight-alpha sRGB.
pub fn decode_pixel(px: [u8; 4]) -> [u8; 4] {
    let t = untable();
    let a = px[3] as usize * 256;
    [
        t[a + px[0] as usize],
        t[a + px[1] as usize],
        t[a + px[2] as usize],
        px[3],
    ]
}

/// Convert a whole RGBA8 buffer in place, layer-texture form to straight alpha.
pub fn decode_buffer(buf: &mut [u8]) {
    debug_assert_eq!(buf.len() % 4, 0, "not a whole number of RGBA pixels");
    for px in buf.as_chunks_mut::<4>().0.iter_mut() {
        *px = decode_pixel(*px);
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
    fn saving_and_reopening_does_not_move_a_pixel() {
        // The invariant the document format rests on. Every byte a layer
        // texture can hold is written out as straight alpha and read back, and
        // has to land on itself: a document that drifted by one level per save
        // would be visibly wrong after an afternoon's work.
        //
        // Not every straight-alpha value survives the other way round — a
        // premultiplied byte at alpha 1 has only fourteen reachable values, so
        // colour there is quantised on the way *in*. That loss belongs to the
        // texture, not to the format, and it has already happened by the time
        // anything here is asked to save.
        // Driven from `encode_pixel` rather than from every (byte, alpha) pair,
        // because those are the values a layer texture can actually contain:
        // premultiplied colour never exceeds its own alpha, and asserting on
        // pairs that cannot occur would only pin down arithmetic on rubbish.
        for a in 0..=255u8 {
            for v in 0..=255u8 {
                let stored = encode_pixel([v, v, v, a]);
                let round = encode_pixel(decode_pixel(stored));
                assert_eq!(round, stored, "colour {v} at alpha {a}");
            }
        }
    }

    #[test]
    fn erased_pixels_stay_erased() {
        assert_eq!(decode_pixel([0, 0, 0, 0]), [0, 0, 0, 0]);
        // Opaque is the identity in both directions.
        assert_eq!(decode_pixel([12, 200, 7, 255]), [12, 200, 7, 255]);
    }

    #[test]
    fn half_alpha_white_comes_back_white() {
        // The inverse of `half_alpha_white_is_linear_not_srgb_half`: the stored
        // ~188 is linear 0.5 premultiplied, which is white at half alpha. A
        // decode that divided in sRGB would give ~215 instead.
        let out = decode_pixel([188, 188, 188, 128]);
        assert!((out[0] as i32 - 255).abs() <= 1, "got {}", out[0]);
    }

    #[test]
    fn a_mask_that_hides_nothing_and_one_that_hides_everything_are_exact() {
        // The two ends have to be exact or every unmasked pixel of an imported
        // mask moves: 255 must reveal completely and 0 must hide completely.
        // They were exact under the old sRGB encode too — the endpoints are the
        // transfer function's fixed points, which is exactly why a fixture whose
        // coverage is only 0 or 255 can see none of what follows.
        assert_eq!(mask_pixel(255), [255, 255, 255, 255]);
        assert_eq!(mask_pixel(0), [0, 0, 0, 255]);
    }

    #[test]
    fn every_coverage_a_source_states_survives_into_the_slice() {
        // **The guard that was missing.** What stood here was
        // `coverage_encoding_is_monotone_and_never_inverts`, and monotone is
        // not injective: `round(linear_to_srgb(v/255)·255)` is monotone and
        // collapses 73 of its 256 inputs onto a neighbour, all of them above
        // input 75, which is the whole upper reveal range. Counting is what
        // catches it, and counting is three lines.
        let mut seen = [false; 256];
        for c in 0..=255u8 {
            let px = mask_pixel(c);
            assert_eq!(px[0], c, "coverage {c} did not survive");
            assert_eq!(px, [c, c, c, 255], "all three channels carry the coverage");
            seen[px[0] as usize] = true;
        }
        assert_eq!(
            seen.iter().filter(|s| **s).count(),
            256,
            "a mask slice must be able to hold every coverage a source states"
        );
    }

    #[test]
    fn a_mask_multiplier_reaches_every_level_the_composite_can_show() {
        // Stated at the far end rather than at this function, because that is
        // where it is spent — and stated for **both** destinations, because a
        // mask scales premultiplied RGBA and the two halves of that land in
        // channels of different kinds. This is the measurement the first draft
        // took only one side of, and one side of it reads as a free win.
        //
        //   destination            linear   sRGB
        //   8-bit linear alpha        256    183
        //   8-bit sRGB colour         183    256
        //
        // Exactly mirrored, and it has to be: the two storages differ by the
        // transfer function and so do the two destinations. What decides it is
        // that a mask is a multiplier on **alpha** — that is the channel it
        // scales, it is what a transparent-background document exports, and it
        // is what every source format's byte already means. The colour column is
        // the cost and is not nothing: a light layer masked down over an opaque
        // dark backdrop bands at the hide end where it did not before, because
        // the first non-zero mask level now takes the output from sRGB 0 to
        // sRGB 13 where it used to step 0, 1, 2.
        let reach = |m: fn(u8) -> f32, out: fn(f32) -> f32| {
            let mut seen = [false; 256];
            for s in 0..=255u8 {
                seen[(out(m(s)) * 255.0 + 0.5) as usize] = true;
            }
            seen.iter().filter(|s| **s).count()
        };
        let linear = |s: u8| s as f32 / 255.0;
        let srgb = |s: u8| srgb_to_linear(s as f32 / 255.0);
        let alpha = |m: f32| m;

        assert_eq!(reach(linear, alpha), 256, "linear storage, alpha");
        assert_eq!(reach(srgb, alpha), 183, "sRGB storage, alpha");
        assert_eq!(reach(linear, linear_to_srgb), 183, "linear storage, colour");
        assert_eq!(reach(srgb, linear_to_srgb), 256, "sRGB storage, colour");
    }

    #[test]
    fn the_mask_buffer_and_the_single_byte_agree() {
        let out = mask_buffer(&[0, 40, 128, 255]);
        assert_eq!(out.len(), 16);
        for (i, c) in [0u8, 40, 128, 255].into_iter().enumerate() {
            assert_eq!(&out[i * 4..i * 4 + 4], mask_pixel(c));
        }
    }

    /// Every stored byte a pre-version-4 mask could hold, put through the
    /// migration — as a real buffer, because that is the only entry point.
    fn v3_converted() -> Vec<u8> {
        let mut buf: Vec<u8> = (0..=255u8).flat_map(mask_pixel).collect();
        decode_v3_mask_buffer(&mut buf);
        buf
    }

    #[test]
    fn a_version_three_mask_keeps_the_multiplier_it_meant() {
        // The migration's whole claim, and it is about the *multiplier* rather
        // than about the byte — the old encode was not injective, so the byte
        // cannot come back and does not need to. What must not move is what the
        // composite multiplies by, and the bound is half a level of the layer's
        // own alpha, which is under the rounding the multiply already does.
        //
        // Swept over the whole domain rather than sampled: the failure this
        // would catch is a wrong direction or a wrong rounding, and both are
        // invisible at the two ends, which are the bytes a fixture reaches for.
        let out = v3_converted();
        for s in 0..=255usize {
            let meant = srgb_to_linear(s as f32 / 255.0);
            let now = out[s * 4] as f32 / 255.0;
            assert!(
                (meant - now).abs() * 255.0 <= 0.5,
                "stored {s} meant {meant} and now reads {now}"
            );
        }
        // The two ends are exact, so a fully revealed or fully hidden mask out
        // of an old document is untouched — which is most of every mask.
        assert_eq!(out[0], 0);
        assert_eq!(out[255 * 4], 255);
        // And it really is a conversion rather than a copy: 188 was the old
        // form's half, and the direction matters — the mirrored bug hands back
        // 229 and makes every old mask *reveal* more than it did.
        assert!(
            (out[188 * 4] as i32 - 128).abs() <= 1,
            "188 meant a half, got {}",
            out[188 * 4]
        );
    }

    #[test]
    fn converting_a_version_three_mask_rewrites_every_colour_channel() {
        // `composite.wgsl` reads `.r` and `docformat`'s writer reads `px[0]`,
        // and nothing promises the next reader will pick the same one, so a
        // conversion that touched only red would leave a trap for whichever is
        // asked next.
        let out = v3_converted();
        for (s, px) in out.as_chunks::<4>().0.iter().enumerate() {
            assert_eq!(px[0], px[1], "stored {s}");
            assert_eq!(px[0], px[2], "stored {s}");
            assert_eq!(px[3], 255, "stored {s} lost its opaque fourth byte");
        }
    }

    #[test]
    fn buffer_and_pixel_paths_agree() {
        let mut buf = vec![10, 20, 30, 40, 255, 128, 0, 200];
        encode_buffer(&mut buf);
        assert_eq!(&buf[0..4], encode_pixel([10, 20, 30, 40]));
        assert_eq!(&buf[4..8], encode_pixel([255, 128, 0, 200]));

        decode_buffer(&mut buf);
        assert_eq!(&buf[0..4], decode_pixel(encode_pixel([10, 20, 30, 40])));
    }
}
