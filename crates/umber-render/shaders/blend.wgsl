// The blend modes, once.
//
// This file is not a shader. It is a prelude, concatenated in front of every
// shader that has to combine two premultiplied colours — `composite.wgsl` for
// the layer stack and the wet stroke, `commit.wgsl` for baking that stroke into
// the layer. There is exactly one statement of what Multiply means, and both
// passes call it.
//
// **That is the point of the file.** CLAUDE.md's rule is that `composite.wgsl`
// and `commit.wgsl` must implement identical blending maths, because the first
// draws the preview and the second replaces it at pointer-up — and any
// difference between them is a stroke that visibly jumps under the artist's
// hand. A rule two files are *disciplined* into keeping is one they will
// eventually stop keeping; a function they both call cannot drift.
//
// Everything here works in **linear** light and on **premultiplied** colour,
// which is what the whole engine holds (see "Colour space" in CLAUDE.md). So a
// layer set to Multiply and a brush set to Multiply are the same arithmetic on
// the same numbers, and mean the same thing.

// Separable blend functions, operating on straight (un-premultiplied) colour.
//
// The numeric values are `umber_core::BlendMode`'s discriminants, consumed
// directly; keep the two in step.
fn blend_rgb(mode: u32, cb: vec3<f32>, cs: vec3<f32>) -> vec3<f32> {
    switch mode {
        case 1u: {  // Multiply
            return cb * cs;
        }
        case 2u: {  // Screen
            return cb + cs - cb * cs;
        }
        case 3u: {  // Overlay — Hard Light with the operands swapped
            let lo = 2.0 * cb * cs;
            let hi = 1.0 - 2.0 * (1.0 - cb) * (1.0 - cs);
            return select(hi, lo, cb <= vec3<f32>(0.5));
        }
        case 4u: {  // Add
            return min(cb + cs, vec3<f32>(1.0));
        }
        default: {  // Normal
            return cs;
        }
    }
}

// W3C compositing, with both operands premultiplied:
//   Co = (1 - ab)*Sc + as*ab*B(Cb, Cs) + (1 - as)*Bc
//   ao = as + ab*(1 - as)
// For Normal this collapses to plain source-over, as it should.
fn composite_over(dst: vec4<f32>, src: vec4<f32>, mode: u32) -> vec4<f32> {
    if (src.a <= 0.0) {
        return dst;
    }
    let cs = src.rgb / src.a;
    let cb = select(vec3<f32>(0.0), dst.rgb / max(dst.a, 1e-5), dst.a > 0.0);
    let blended = blend_rgb(mode, cb, cs);

    let co = (1.0 - dst.a) * src.rgb + src.a * dst.a * blended + (1.0 - src.a) * dst.rgb;
    let ao = src.a + dst.a * (1.0 - src.a);
    return vec4<f32>(co, ao);
}
