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

// Curves several of the modes below are written in terms of, so each exists
// once. Hard Light *is* Overlay with the operands swapped, and Vivid Light is
// the dodge and burn curves either side of the midpoint — writing those out
// again would be three more places for one formula to drift.
fn b_screen(cb: vec3<f32>, cs: vec3<f32>) -> vec3<f32> {
    return cb + cs - cb * cs;
}

fn b_hard_light(cb: vec3<f32>, cs: vec3<f32>) -> vec3<f32> {
    let lo = 2.0 * cb * cs;
    let hi = b_screen(cb, 2.0 * cs - 1.0);
    return select(hi, lo, cs <= vec3<f32>(0.5));
}

fn b_color_dodge(cb: vec3<f32>, cs: vec3<f32>) -> vec3<f32> {
    // W3C's three cases. The backdrop being zero stays zero — otherwise a
    // division by `1 - cs` at `cs == 1` would take black to white.
    let dodged = min(vec3<f32>(1.0), cb / max(1.0 - cs, vec3<f32>(1e-5)));
    let atop = select(dodged, vec3<f32>(1.0), cs >= vec3<f32>(1.0));
    return select(atop, vec3<f32>(0.0), cb <= vec3<f32>(0.0));
}

fn b_color_burn(cb: vec3<f32>, cs: vec3<f32>) -> vec3<f32> {
    let burned = 1.0 - min(vec3<f32>(1.0), (1.0 - cb) / max(cs, vec3<f32>(1e-5)));
    let atop = select(burned, vec3<f32>(0.0), cs <= vec3<f32>(0.0));
    return select(atop, vec3<f32>(1.0), cb >= vec3<f32>(1.0));
}

// The non-separable four, W3C's own definitions.
//
// **The luma weights are W3C's 0.3/0.59/0.11 and this engine blends in linear
// light**, which is a deviation worth naming rather than discovering: those
// constants were chosen for gamma-encoded colour. Applying them to linear
// values is what Umber does with every other mode too — Multiply here is
// already not Photoshop's Multiply, because this file works where the rest of
// the engine works (see "Colour space" in CLAUDE.md). Consistency with the
// modes that were already here is worth more than agreement with a different
// application's colour space, and changing it would move every existing
// document's Multiply layers.
fn lum(c: vec3<f32>) -> f32 {
    return 0.3 * c.r + 0.59 * c.g + 0.11 * c.b;
}

fn clip_color(c: vec3<f32>) -> vec3<f32> {
    let l = lum(c);
    let n = min(c.r, min(c.g, c.b));
    let x = max(c.r, max(c.g, c.b));
    var out = c;
    if (n < 0.0) {
        out = l + (out - l) * l / max(l - n, 1e-5);
    }
    if (x > 1.0) {
        out = l + (out - l) * (1.0 - l) / max(x - l, 1e-5);
    }
    return out;
}

fn set_lum(c: vec3<f32>, l: f32) -> vec3<f32> {
    return clip_color(c + (l - lum(c)));
}

fn sat(c: vec3<f32>) -> f32 {
    return max(c.r, max(c.g, c.b)) - min(c.r, min(c.g, c.b));
}

// W3C sets the least channel to 0, the greatest to `s` and the middle one
// proportionally. Rescaling the whole vector does exactly that without having
// to sort the channels, which in WGSL would be six branches for no gain.
fn set_sat(c: vec3<f32>, s: f32) -> vec3<f32> {
    let mn = min(c.r, min(c.g, c.b));
    let mx = max(c.r, max(c.g, c.b));
    if (mx > mn) {
        return (c - mn) * s / (mx - mn);
    }
    return vec3<f32>(0.0);
}

// The separable and non-separable blend functions, on straight
// (un-premultiplied) colour.
//
// The case numbers are `umber_core::BlendMode`'s discriminants, consumed
// directly; keep the two in step. `all_lists_every_blend_mode` guards the
// Rust half and `every_blend_mode_moves_the_picture` guards this one — a
// variant added with no arm here falls to `default` and composites as Normal,
// silently, which is a mode the interface offers and the engine ignores.
fn blend_rgb(mode: u32, cb: vec3<f32>, cs: vec3<f32>) -> vec3<f32> {
    switch mode {
        case 1u: {  // Multiply
            return cb * cs;
        }
        case 2u: {  // Screen
            return b_screen(cb, cs);
        }
        case 3u: {  // Overlay — Hard Light with the operands swapped
            return b_hard_light(cs, cb);
        }
        case 4u: {  // Add, which is Photoshop's Linear Dodge
            return min(cb + cs, vec3<f32>(1.0));
        }
        case 5u: {  // Darken
            return min(cb, cs);
        }
        case 6u: {  // Lighten
            return max(cb, cs);
        }
        case 7u: {  // Colour Dodge
            return b_color_dodge(cb, cs);
        }
        case 8u: {  // Colour Burn
            return b_color_burn(cb, cs);
        }
        case 9u: {  // Linear Burn
            return max(cb + cs - 1.0, vec3<f32>(0.0));
        }
        case 10u: {  // Hard Light
            return b_hard_light(cb, cs);
        }
        case 11u: {  // Soft Light — W3C's piecewise curve
            let d = select(
                sqrt(cb),
                ((16.0 * cb - 12.0) * cb + 4.0) * cb,
                cb <= vec3<f32>(0.25),
            );
            let dark = cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb);
            let light = cb + (2.0 * cs - 1.0) * (d - cb);
            return select(light, dark, cs <= vec3<f32>(0.5));
        }
        case 12u: {  // Vivid Light — burn below the midpoint, dodge above
            let lo = b_color_burn(cb, 2.0 * cs);
            let hi = b_color_dodge(cb, 2.0 * cs - 1.0);
            return select(hi, lo, cs <= vec3<f32>(0.5));
        }
        case 13u: {  // Linear Light
            return clamp(cb + 2.0 * cs - 1.0, vec3<f32>(0.0), vec3<f32>(1.0));
        }
        case 14u: {  // Pin Light
            let lo = min(cb, 2.0 * cs);
            let hi = max(cb, 2.0 * cs - 1.0);
            return select(hi, lo, cs <= vec3<f32>(0.5));
        }
        case 15u: {  // Difference
            return abs(cb - cs);
        }
        case 16u: {  // Exclusion
            return cb + cs - 2.0 * cb * cs;
        }
        case 17u: {  // Subtract
            return max(cb - cs, vec3<f32>(0.0));
        }
        case 18u: {  // Divide
            return min(cb / max(cs, vec3<f32>(1e-5)), vec3<f32>(1.0));
        }
        case 19u: {  // Hue
            return set_lum(set_sat(cs, sat(cb)), lum(cb));
        }
        case 20u: {  // Saturation
            return set_lum(set_sat(cb, sat(cs)), lum(cb));
        }
        case 21u: {  // Colour
            return set_lum(cs, lum(cb));
        }
        case 22u: {  // Luminosity
            return set_lum(cb, lum(cs));
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
//
// **Add (Glow) is the one mode that is not a blend function**, which is why it
// is here and not a `blend_rgb` arm. Every other mode is some `B(Cb, Cs)`
// dropped into the formula above; this one changes the *compositing* step —
// it is Porter-Duff `plus`, a straight addition of premultiplied colour, which
// is also exactly what OpenRaster's `svg:plus` names. Clip Studio's own "Add"
// is the ordinary blend function and is `BlendMode::Add`.
//
// **The two agree wherever the backdrop is opaque or empty**, and that is
// derived rather than assumed. With ab = 1 the general form gives
// `Cb + as*Cs` before clamping, and `plus` gives `Bc + Sc` = `Cb + as*Cs`;
// with ab = 0 both give `Sc`. So they can only differ where the backdrop is
// *partly* transparent — a soft edge — which is what the importer's note about
// this pair has always said.
fn composite_over(dst: vec4<f32>, src: vec4<f32>, mode: u32) -> vec4<f32> {
    if (mode == 23u) {  // Add (Glow)
        return min(dst + src, vec4<f32>(1.0));
    }
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
