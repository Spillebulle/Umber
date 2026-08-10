//! The canvas renderer: layer storage, stroke scratch surface and the three
//! pipelines that move data between them.

use bytemuck::{Pod, Zeroable};
use glam::{UVec2, Vec2};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use umber_core::{
    Affine, Anchor, Background, BlendMode, BrushMode, Camera, CanvasCopy, Color, Dab, Effect,
    EffectKind, FlipAxis, OutlinePosition, PixelRect, Selection, TipMask, transform,
};
use wgpu::util::DeviceExt;

/// Layer storage format.
///
/// `Srgb`, despite the engine working in linear throughout, because eight bits
/// of *linear* storage spends nearly all its precision on highlights: a dark
/// ink at linear 0.0056 lands on 1–2 of 255, so dark tones band badly and drift
/// a couple of sRGB levels between the float preview and the stored result. An
/// sRGB-typed target distributes precision perceptually. Blending stays correct
/// — the hardware decodes to linear, blends, and re-encodes on write.
const LAYER_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// What one pixel of [`LAYER_FORMAT`] costs. Four, and it is named because
/// [`grown_capacity`] reasons in bytes and a bare `* 4` beside a canvas size
/// reads as arbitrary.
const LAYER_BYTES_PER_PIXEL: u64 = 4;

/// The same bits, viewed without the transfer function.
///
/// Used by one pass and one pass only: [`CanvasRenderer::flip_layers`], which
/// has to be an exact permutation of texels. Reading through an sRGB view
/// decodes to linear and writing through one re-encodes, and a round trip
/// through that pair is a promise about rounding rather than about pixels —
/// which matters here more than anywhere else in the renderer, because undoing
/// a flip *is* another flip, so any drift compounds every time. Read as raw
/// `u8 / 255` and written back, an f32 carries the byte exactly.
///
/// Listed in the layer array's `view_formats`, which is what makes such a view
/// legal at all.
const LAYER_FORMAT_LINEAR: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// The stroke scratch only needs coverage, so one channel instead of four —
/// a 4x saving on the bandwidth of the hottest texture in the frame.
///
/// Eight bits because that is exactly the width of where the coverage is going:
/// `LAYER_FORMAT`'s alpha channel is linear 8-bit (an sRGB format encodes RGB
/// only), so commit re-quantises to 256 levels whatever the scratch held. The
/// scratch therefore adds no loss of its own, and widening it cannot make the
/// pen's 1024 pressure levels reach the canvas — only a wider *layer* could.
/// `a_pressure_step_finer_than_the_layer_makes_no_mark` pins that, and the
/// build-up target below has the one case where the width is not a wash.
///
/// Shared with the tip and grain masks, which are 8-bit source data anyway.
const STROKE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;

/// Instance buffer capacity, in dabs. A single frame only ever holds the dabs
/// generated since the last frame; 64k is far more than a 120 Hz frame of
/// even the fastest flick can produce.
const MAX_DABS_PER_FRAME: usize = 65_536;

/// Stack entries a document may hold. Mirrored by `LayerStack::MAX`.
///
/// It no longer sizes anything in `composite.wgsl` — see [`MAX_DRAWS`], which
/// does. What it bounds is the *stack*: layers and folders, the thing the
/// layers panel lists.
const MAX_LAYERS: usize = 64;

/// How deep the layer texture array may grow — **and the number every other
/// capacity in this block is derived from, because it is the only one that is
/// not ours to pick.**
///
/// A slice is a layer, a layer's mask, the spare a floating transform previews
/// into, or one baked effect. Mirrored by `LayerStack::MAX_SLOTS`.
///
/// `Gpu::new` requests `Limits::downlevel_defaults().using_resolution(…)`.
/// `downlevel_defaults` names the three texture *dimensions*, four buffer and
/// shader limits, and then falls through to `Limits::defaults()` — so
/// `max_texture_array_layers` is **256**, and `using_resolution` copies only
/// those three dimensions and cannot raise it. A 257th slice is a
/// `create_texture` validation error, and a validation error reaches
/// `crash::device_error`, which is fatal with a painting on screen.
///
/// `docs/layer-effects.md` §6.3 derived 257 — 64 layers, 64 masks, one float
/// spare and 128 effects — without checking that. It is one over, which is
/// exactly why nobody had looked: the previous ceiling of 129 sat 127 clear.
/// The assertion below is therefore the load-bearing part of this block, not
/// the comment.
const MAX_SLOTS: usize = 256;

/// **The ceiling is the device's, so it is read from the device's own limits
/// rather than restated.**
///
/// `Limits::downlevel_defaults` is a `const fn`, which is what lets this be a
/// compile error instead of a test. It has to be one: what it catches is not a
/// red test run but `create_texture` failing validation inside
/// [`CanvasRenderer::ensure_slots`], which goes through `on_uncaptured_error`
/// and takes the process down.
const _: () = assert!(
    MAX_SLOTS <= wgpu::Limits::downlevel_defaults().max_texture_array_layers as usize,
    "the layer array would be deeper than downlevel_defaults guarantees"
);

/// A full stack of masked layers and the float's spare must fit under the
/// ceiling, or [`MAX_EFFECT_SLICES`] underflows.
///
/// The underflow would already be a compile error, since these are consts, and
/// this does not claim to be reported first — both items are evaluated and
/// rustc chooses. What it adds is a *message*, where an arithmetic-overflow
/// diagnostic against an expression says nothing about which of two capacities
/// was set wrong.
///
/// Written `* 2 <` rather than the `* 2 + 1 <=` the sentence above describes,
/// because clippy's `int_plus_one` rejects the second spelling. They are the
/// same predicate on integers; the `+ 1` is the float's spare.
const _: () = assert!(
    MAX_LAYERS * 2 < MAX_SLOTS,
    "no room under the ceiling for a fully masked stack and a float"
);

/// What is left of [`MAX_SLOTS`] once every layer, every mask and the float's
/// spare have their slice: **127**.
///
/// Derived rather than written down, so a change to [`MAX_LAYERS`] carries
/// through instead of leaving numbers to be edited by hand. The float's spare
/// is inside the subtraction and never gives way — a transform must always have
/// somewhere to preview.
///
/// **Raising [`MAX_LAYERS`] *lowers* this, and lowers [`MAX_DRAWS`] with it.**
/// The ceiling is fixed by the device, so every layer added takes two slices out
/// of the effect budget. This is `MAX_SLOTS - (MAX_LAYERS * 2 + 1)` — 55 at a
/// stack cap of 100 and **1** at 127 — while `MAX_DRAWS` is
/// `MAX_SLOTS - MAX_LAYERS - 1`, which is 155 and 128 at the same two. They are
/// different quantities and this comment ran them together, quoting the effect
/// budget's 1 as though it were the draw cap; at 127 layers `MAX_DRAWS >=
/// MAX_LAYERS` is true because it is 128, not because 1 would have satisfied it.
/// The test loop computes both.
///
/// **Something does fail when it happens**, and this comment claimed otherwise.
/// Raising `umber-core`'s `LayerStack::MAX` is a **compile** error:
/// `effect::BUDGET_DERIVATION` is a `const` assertion over it, naming the
/// reason. Raising [`MAX_LAYERS`] *here* alone is a **test** failure instead,
/// `the_three_draw_capacities_agree`'s equality against that constant — worth
/// separating, because the whole point of preferring the first is that a
/// compile error cannot be skipped. Either way somebody is told. The original
/// claim was true when it was written, before the model landed, and false from
/// the merge onwards; anyone raising the stack cap still has to decide whether
/// the effect budget left is one worth having.
///
/// It also does not carry through to the shader, which holds `MAX_DRAWS` as a
/// literal `191u`. That is a fourth number and it is changed by hand;
/// `the_three_draw_capacities_agree` is what makes forgetting it a red test
/// rather than a silent uniform mismatch.
///
/// **127 rather than 128 is also what makes the cap reachable**, which
/// `docs/layer-effects.md` §6.3 records and is worth repeating where the number
/// is: with two effect kinds and one of each per layer, 64 layers can enable at
/// most 128, so against a budget of 128 the refusal sits exactly on the ceiling
/// and can only be exercised by a stack the model forbids. Against 127 the last
/// effect on a fully doubled stack is refused for real. **Re-check that when an
/// effect kind is added** — the arithmetic moves and nothing here will say so.
const MAX_EFFECT_SLICES: usize = effect_slices(MAX_LAYERS, MAX_SLOTS);

/// Entries the composite pass's two uniform arrays carry, mirrored by
/// `MAX_DRAWS` in `composite.wgsl`: **191**.
///
/// **A draw is not a stack entry**, which is why this is not [`MAX_LAYERS`].
/// One layer composites as one draw today; a layer carrying effects composites
/// as several, each with its own slot, opacity and blend mode, because a shadow
/// at Multiply has to multiply against what is *under* the layer.
/// `docs/layer-effects.md` §6.2 has the argument.
///
/// **It is `MAX_LAYERS + MAX_EFFECT_SLICES` and not a round number**, because
/// an effect draw reads an effect *slice* — one draw, one slice — so the draw
/// budget cannot exceed the slice budget. §6.2 says 192, which was derived from
/// the 257 the ceiling refuses; a 192nd entry would be a draw with nowhere to
/// read from.
///
/// The cost of raising it is uniform bytes and the upload, and nothing per
/// fragment: the loop in `composite.wgsl` is bounded by `layer_count`. The
/// bytes are counted in [`ViewUniforms`].
const MAX_DRAWS: usize = MAX_LAYERS + MAX_EFFECT_SLICES;

/// The jump flood's seed coordinate.
///
/// A *coordinate*, so it has to be exact: an `f16`'s mantissa runs out of whole
/// integers at 2048, which is a canvas size Umber ships with, and a 2049th
/// column would flood towards the wrong texel. Sixteen bits of unsigned integer
/// covers every canvas `max_texture_dimension_2d` permits, which is also what
/// lets 65535 stand for "no seed" without ever colliding with one — the margin
/// was four times over at a 16384 ceiling and is twice over at
/// `Document::MAX_EDGE`'s 32768, where the largest coordinate is 32767. A
/// device reporting 65536 would be the one that breaks the sentinel, and none
/// does.
///
/// **A render target on every device, and that was checked rather than
/// assumed.** `TextureFormat::guaranteed_format_features` gives `Rg16Uint`
/// `attachment` on `Features::empty()`, so it is part of what the WebGPU
/// specification requires rather than something an adapter may refuse — which is
/// the question that had to be asked, because the whole point of requesting
/// `downlevel_defaults` is that a desktop build must not depend on what a mobile
/// GPU will not do. `the_seed_format_is_a_render_target_on_every_device` is the
/// pin; a wgpu bump that moved it would otherwise be a fatal `create_texture`
/// validation error on somebody else's machine and on nobody's here.
///
/// The first draft carried an `effects_available(&Adapter)` beside this instead,
/// and it was never called — a control that would have greyed itself out if
/// anything had asked it. An uncalled guard is a promise nothing keeps;
/// `examples/measure-effects.rs` still asks the adapter, which is the right place
/// for the caution, because a measuring tool must not die on the first
/// allocation a device refuses.
const SEED_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg16Uint;

/// How much smaller than the canvas a wide blur runs.
///
/// Sixteen times fewer texels at a quarter of the radius is ~64x less work, and
/// `docs/layer-effects.md` §3.2 is emphatic that this is not an optimisation to
/// add later: at 10000² a full-resolution tent goes from 8.5 ms at radius 4 to
/// 83 ms at radius 64, where the downsampled one is 2.0 to 3.2 ms across the
/// same sweep.
const EFFECT_DOWN: u32 = 4;

/// Below this softness the blur runs at full resolution instead.
///
/// **A departure from §3.2, which says the blur is always downsampled, and the
/// reason is that a quarter-resolution tent cannot represent a narrow soft edge
/// at all.** Two box passes of `2r + 1` taps at a reduction of `d` have a half
/// support of `d(2r + 1)`, so the smallest edge [`EFFECT_DOWN`] can draw is
/// twelve document pixels and the quantisation step is eight — a shadow asked
/// for four would arrive at twelve, and one asked for sixteen could only be
/// twelve or twenty. §13 left "whether the quarter-resolution blur can be
/// calibrated at 20 px" open; this is that question answered in the honest
/// direction rather than left to the artist to discover.
///
/// It is a *parameter* and not a second implementation — the same four passes
/// run either way, and `fs_down` is simply not recorded — which is why it is not
/// the second path §3.1a refuses for the distance field. At and above this
/// figure the quantisation is under 13% of the radius, which is invisible on a
/// falloff; below it the full-resolution tent is exact to a pixel and costs
/// 1.3 ms at 2048² by §3.4's own table.
const EFFECT_FULL_RES_SOFTNESS: f32 = 32.0;

/// The largest canvas a bake may be re-run on **every frame**, in pixels.
///
/// `docs/layer-effects.md` §5.1 and §12: at 2048² a bake is 0.4 ms to 1.2 ms, so
/// rebaking from layer plus scratch every frame is affordable and the shadow
/// follows the brush. At 10000² the jump flood alone is 19 ms and holds about a
/// gigabyte, so above this the bake waits for the layer's pixels to change —
/// which at the end of a stroke is the commit. The shadow then lags a stroke,
/// and **nothing on screen says so**, because a shadow one stroke late is still
/// the right picture; a notice about it would be a notice on every frame of
/// every stroke on a large canvas.
///
/// **Measured on the shipped bake and not on the design's prototypes** —
/// `examples/measure-effects.rs`'s second table, which exists because those are
/// two different measurements and quoting one for the other is the mistake §3.1a
/// records. On an RTX 3080:
///
/// | | 2048² | 10000² |
/// |---|---|---|
/// | shadow at its default 5 px | 0.6 | 7.6 |
/// | shadow at 64 px | 0.3 | 4.1 |
/// | outline 16 px, outside | 1.2 | **20.0** |
/// | outline 16 px, centre | 1.8 | **33.9** |
///
/// **One decimal at 2048², because §3.4's own caveat is that back-to-back runs
/// vary by 4x at those magnitudes** — that is a property of timing a submit and a
/// fence, not of the code. What the column says is that every bake there is
/// comfortably inside a frame.
///
/// The centred outline is the worst there is because it floods **twice** — the
/// outward distance and the inward one — which is the shape a knockout per side
/// of the stack hid, since Centre was then an Outside of half the width.
///
/// So 2048² is free and 10000² is not, which is the split §3.4 predicted. 4096²
/// is where the line is drawn and it is a judgement rather than a reading:
/// nothing has been measured between those two sizes and the flood scales with
/// the area, which puts a 4096² outline at roughly 3 ms and a centred one at 6.
///
/// **This bounds the canvas; [`CanvasRenderer::effect_bakes_live`] is the gate,
/// and it asks per effect.** §5.1's corrected claim is that what cannot hold at
/// canvas scale is "memory at canvas scale and the stroke's distance field, not
/// the shadow and not the frame budget", and the table bears it out: at 10000²
/// both outlines are over a 60 Hz frame and neither shadow is.
///
/// A single gate on the canvas is what shipped first and is worse in a way an
/// artist cannot see — one expensive outline anywhere in the stack would switch
/// the live rebake off for a cheap shadow on another layer, so a shadow following
/// the brush would depend on a setting somewhere else in the document. Above this
/// figure the per-effect gate keeps exactly the bakes that fit.
///
/// Stage 3's region-bounded rebake is what removes the need for any of it.
const EFFECT_LIVE_PIXELS: u64 = 4096 * 4096;

/// The largest destination a block of placed text may be **re-rasterised into
/// every frame** of a transform drag.
///
/// `docs/text-tool.md` §4(c): scaling placed text has to be a fresh
/// rasterisation, because `Float` scales by sampling and text is the one thing
/// that cannot tolerate a bilinear resample. The cost is then the *destination*
/// area, which is unbounded in the way a drag is unbounded, so it needs a
/// budget. Measured by `umber-core/examples/measure-text.rs` — re-run it rather
/// than trusting this table — on a 13th-generation laptop CPU, release. **The
/// figures are the fastest of nine runs, not the median**, and that is not a
/// stylistic difference: a median resists an outlier and does not resist
/// sustained load, and the first table published here was taken while another
/// crate was building and overstated the worst row by 1.6x. `timings` in that
/// example is where the choice is argued.
///
/// | block | destination | Mpx | rasterise | ms/Mpx |
/// |---|---|---|---|---|
/// | "Umber" at 72 px, placed | 212×54 | 0.011 | 0.10 ms | 9.6 |
/// | the same, dragged to 4x | 848×213 | 0.17 | 0.90 ms | 5.2 |
/// | the same, to 8x | 1695×424 | 0.69 | 3.47 ms | 5.1 |
/// | the same, to 16x | 3390×848 | 2.74 | 12.24 ms | 4.5 |
/// | three lines at 72 px, placed | 747×223 | 0.16 | 0.85 ms | 5.3 |
/// | the same, to 2x | 1495×445 | 0.63 | 2.65 ms | 4.2 |
/// | the same, to 4x | 2988×890 | 2.54 | 9.66 ms | 3.8 |
/// | the same, to 8x | 5974×1780 | 10.1 | 36.33 ms | 3.6 |
/// | "Umber" at 1000 px, placed | 2943×735 | 2.06 | 8.99 ms | 4.4 |
///
/// Every row this doc reasons from is in that table, which is worth stating
/// because the first version of it cited four figures that were not in it and
/// told the reader to "read off the rows above".
///
/// **The rate is not constant, and it is worst for *small* blocks** — the
/// per-glyph setup has nothing to amortise over, which is why the placed caption
/// reads 9.6 while everything over half a megapixel sits between 3.6 and 5.3.
/// Shaping is 0.01 to 0.07 ms and does not move with the scale at all, which is
/// what says the budget belongs on the area and not on a cache of shaped runs.
///
/// So a megapixel is **about 5 ms**, read off the two rows either side of it
/// (0.69 Mpx at 5.1 ms/Mpx and 2.74 at 4.5), and it leaves two thirds of a 60 Hz
/// frame for everything else.
///
/// **Two megapixels is about 9 ms and is *inside* a frame, so "the next power of
/// two does not fit" is not the argument and this doc claimed it was.** The
/// argument is the remainder: 9 ms leaves 7.7 ms for egui to lay out and
/// tessellate the whole interface, for the upload of 8 MB, and for the composite
/// — where 5 ms leaves 11.7. **What the interface itself costs has not been
/// measured**, so the margin is a judgement and not a subtraction; it is a
/// judgement at the same place [`EFFECT_LIVE_PIXELS`]'s is, and if somebody
/// measures the frame properly this is the constant that should move.
///
/// **[`text::MAX_PIXELS`](umber_core::text::MAX_PIXELS) does not stand in for
/// this and that is the whole reason this exists.** At the 3.6 ms/Mpx the largest
/// row measures, its 16 megapixels is about 58 ms of rasterisation: three and a
/// half frames. The cap bounds an allocation and leaves the drag unbounded.
/// (Two commit messages on this branch give that figure as 110 ms and then 63;
/// both multiplied the cap by a rate measured on smaller blocks, the second from
/// a loaded run. The conclusion never changed and a commit message cannot be
/// amended, so the arithmetic lives here.)
///
/// **What it bites on is a drag and never a placement.** The caption at its own
/// size is eleven kilopixels — ninety times under this — and the paragraph is six
/// times under. Reaching it means having dragged a corner to several times the
/// size the text was set at.
///
/// # It degrades latency, and deliberately not quality
///
/// `docs/layer-effects.md` §6.1a's distinction, and this is the *degrading*
/// kind — [`text::MAX_PIXELS`](umber_core::text::MAX_PIXELS) is the refusing one,
/// which is why the two are not the same number and do not live in the same
/// crate. `MAX_PIXELS` bounds an *allocation* and answers with an error a notice
/// can name. This bounds a *frame* and cannot refuse: an artist dragging a
/// corner has not asked a question that "no" is an answer to.
///
/// **§4(c) recommends falling back to the sampler above the budget and that is
/// unsound, for the reason §4(c) itself gives for refusing its own first
/// option.** Follow it to the commit: either the sampled pixels are what lands
/// in the layer, and a block that is merely large holds permanently soft text —
/// the pixelation bug this whole path exists to remove, moved to large sizes and
/// made silent — or the commit re-rasterises, and the picture snaps from soft to
/// sharp the instant the artist lets go, which is the option §4(c) refuses by
/// name as the stroke-jump bug in a different hat. There is no third choice,
/// because `render_float` is one function called twice: whatever the preview
/// samples *is* what commits.
///
/// So the degradation is in **when** the re-rasterisation runs, never in what it
/// produces. Above this figure it runs when the matrix settles rather than on
/// every frame of the drag; a release stops the matrix, so the next frame catches
/// up and the commit is always a rasterisation of the matrix it is committing.
/// Whatever is on screen is always a true rasterisation of *some* matrix, which is
/// what makes it a latency cost rather than a quality one.
///
/// # What deferring actually looks like, which is not a lagging shadow
///
/// This doc said "exactly the shape [`EFFECT_LIVE_PIXELS`] already uses, where
/// the bake waits for the pixels to change and the shadow lags a stroke", and
/// that comparison is **too flattering and the difference matters to whoever
/// writes the caller.**
///
/// A late drop shadow is still attached to the layer it belongs to; it is the
/// right picture, a stroke behind. A deferred text rasterisation is not: the
/// float's pixels sit where the *previous* matrix put them while the transform
/// box is drawn at the current one, so the text visibly **comes away from its own
/// handles** and slides back when the hand stops. That is a worse artefact than a
/// lag, and it is the thing this policy trades the blur for.
///
/// It also is not "a frame or two". The deferral lasts as long as the
/// rasterisation takes, which at the cap is about 58 ms — so at the sizes this
/// gate bites, the box can be most of a tenth of a second ahead of the picture.
///
/// **Whether that is the right trade is the caller's decision and it has not been
/// made**, because there is no caller. Two options are open to it and neither is
/// settled here: draw the box at the *stale* matrix too, so the two agree and the
/// whole float lags together as the shadow does; or keep the box live and accept
/// the detachment. The first is honest and makes the handles feel heavy; the
/// second keeps the handles crisp and lets the picture trail them. What is
/// settled is only the part this constant owns — that the pixels are never soft.
///
/// **Nothing on screen says anything either way**, which is the one part the
/// shadow's reasoning does carry over: a picture that is a moment behind is still
/// the right picture, where a picture that is soft is not, and a notice would fire
/// on every frame of every large drag to report that Umber is keeping its promise.
const TEXT_RESET_LIVE_PIXELS: u64 = 1024 * 1024;

/// May a block of text landing in `dest` be re-rasterised on **this** frame of a
/// drag, or does it wait for the matrix to settle?
///
/// See [`TEXT_RESET_LIVE_PIXELS`] for the figure, for why the answer above it is
/// "later" rather than "blurrily", and for what deferring actually looks like —
/// which is not the lagging drop shadow it is tempting to compare it to. The
/// per-block question rather than one gate on the canvas, for
/// [`CanvasRenderer::effects_bake_live`]'s reason: what costs the frame is the
/// area the text covers, and a large canvas holding a caption should
/// re-rasterise it every frame.
///
/// **Nothing calls this yet.** It is `pub` because the decision belongs in this
/// crate — beside the constant it reads, and beside the upload it bounds — while
/// the caller is `app.rs`'s transform region, which cannot be written until a
/// text float carries the block it was set from. `Editor::float` is a `Copy`
/// `Floating` today and a `TextBlock` is a `String`, so that is a change to
/// `editor.rs` and it is somebody else's. Until it lands, a text float still
/// scales by sampling and still comes out soft: this bounds nothing, and says so
/// rather than being written as though it were in force. The form is
/// `Transform::reseat`'s and the reason is the one
/// [`CanvasRenderer::effects_bake_live`] gives for the same shape.
///
/// A pure function of a rectangle, so the policy is testable without a device —
/// the arrangement [`band_rows`] and [`grown_capacity`] already keep.
pub fn text_reset_is_live(dest: PixelRect) -> bool {
    dest.area() <= TEXT_RESET_LIVE_PIXELS
}

/// Slices left for effects once a stack of `layers`, all masked, and the
/// float's spare have theirs, under a ceiling of `ceiling`.
///
/// **A `const fn` rather than an expression inline in [`MAX_EFFECT_SLICES`],
/// and that is what makes the derivation testable at all.** A test that
/// recomputes `ceiling - (layers * 2 + 1)` for itself and then asserts
/// `layers * 2 + 1 + that == ceiling` has written `a - b + b == a`, which holds
/// for every function body there is — that was the second draft of
/// `the_slice_ceiling_agrees_with_umber_core` and it tested nothing. Calling
/// *this* turns the same assertion into a statement about the rule: a body
/// correct at 64 and wrong elsewhere fails it, which was checked by writing
/// one.
const fn effect_slices(layers: usize, ceiling: usize) -> usize {
    ceiling - (layers * 2 + 1)
}

/// Texture-array slices allocated up front, **before** the byte budget is
/// consulted. Growth doubles from here while the array is cheap, so a typical
/// document never pays for a copy — see [`grown_capacity`].
///
/// Four is only cheap on a canvas where a slice is. Taken literally it is
/// 1.53 GiB at 10000², of which 1.12 GiB is speculation on behalf of a document
/// that has one layer — over four times the 256 MiB [`grown_capacity`] bounds
/// itself by, decided twenty lines from the constant that states that bound. So
/// it goes through [`initial_slots`], which is the same budget applied to the
/// same question.
const INITIAL_SLOTS: u32 = 4;

/// What one slice of a canvas this size costs.
///
/// One statement of it, because [`initial_slots`] and
/// [`CanvasRenderer::ensure_slots`] both budget against it and two spellings
/// would be two chances to budget against different numbers. Saturating,
/// though `max_texture_dimension_2d` keeps a canvas four orders of magnitude
/// below where this could wrap: the value only ever decides whether to
/// speculate, so a saturated one reads as "far too big to speculate on", which
/// is the right answer to an impossible canvas — where a debug-build panic, or
/// a release-build *wrap* to something small enough to authorise unlimited
/// doubling, would be the wrong two.
fn slice_bytes(doc_size: UVec2) -> u64 {
    u64::from(doc_size.x)
        .saturating_mul(u64::from(doc_size.y))
        .saturating_mul(LAYER_BYTES_PER_PIXEL)
}

/// [`INITIAL_SLOTS`], but never more speculation than the growth budget allows.
///
/// A renderer holds no pixels when this is asked, so allocating fewer costs a
/// copy the first time a second layer arrives and nothing else — where
/// allocating more is a gigabyte nobody asked for, on the canvas where every
/// other path here has already decided not to speculate.
fn initial_slots(slice_bytes: u64) -> u32 {
    let affordable = GROWTH_DOUBLING_BUDGET_BYTES
        .checked_div(slice_bytes)
        .unwrap_or(u64::from(INITIAL_SLOTS))
        .clamp(1, u64::from(INITIAL_SLOTS)) as u32;
    affordable.min(INITIAL_SLOTS)
}

/// How large the layer array may grow **by doubling**.
///
/// Doubling is the right way to grow a collection whose elements are cheap and
/// the wrong way to grow one whose elements are canvas-sized. A slice is
/// `width × height × 4`, so the same "one more slice" costs 256 KiB at 256² and
/// 400 MB at 10000², and the overshoot is what a budget counted in *slices*
/// cannot see. Stating it in bytes is what makes the policy canvas-aware.
///
/// A quarter of a gigabyte of speculative texture is the trade: enough that an
/// ordinary document never reallocates twice for the same layer, small enough
/// that the waste can never dominate a working set. At 2048² it allows doubling
/// to 16 slices; at 10000² it allows none, which is correct — nothing should
/// speculatively allocate 400 MB.
///
/// **Two paths spend slices and only these two consult this.**
/// [`grown_capacity`] does, and [`initial_slots`] does.
/// [`CanvasRenderer::resize`] does **not** — it carries the old canvas's slice
/// count onto the new one, where the same count is a different number of bytes.
/// That predates this rule and cannot be fixed from inside `resize`; the whole
/// case is written up there.
///
/// **What it actually bounds is the overshoot, at itself**, and it bounds both
/// halves of [`grown_capacity`] separately. While doubling: the loop runs only
/// while the resulting array is inside this, so `capacity × slice ≤ this`, and
/// it stops the first time `capacity >= needed`, so `capacity < 2 × needed` and
/// the waste is under `capacity / 2` — half a budget. Past it: the waste is at
/// most one slice short of a whole quantum, and a quantum is by construction
/// the most slices that fit in this. So 256 MiB, on any canvas, at any slice
/// count. Measured worst is 252 MiB, at 1024² reaching for a 65th slice.
/// `the_overshoot_is_bounded_by_the_budget_on_every_canvas` sweeps it, over
/// every starting capacity as well as every target, because the bound is a
/// statement about the call and not about a cold start.
const GROWTH_DOUBLING_BUDGET_BYTES: u64 = 256 << 20;

/// The capacity to allocate so that `needed` slices exist, given how large one
/// slice is.
///
/// **Double while the resulting array would stay inside
/// [`GROWTH_DOUBLING_BUDGET_BYTES`]; past that, round `needed` up to a whole
/// [`growth_quantum`].**
///
/// A pure function so the policy is testable without a device, which is the
/// arrangement [`band_rows`] already keeps and the only one that works here: the
/// case that matters is a 129th slice at 2048², and allocating it for real is
/// two gigabytes of texture nobody can ask a CI runner for.
///
/// **This exists because raising `MAX_SLOTS` from 129 to 256 silently changed
/// what `.min(MAX_SLOTS)` did.** That clamp had been acting as a *tight* bound
/// on the overshoot: at a ceiling of 129 a document needing its 129th slice
/// doubled from 128 to 256 and was clamped straight back to 129. At a ceiling of
/// 256 the same document gets 256 — 4.29 GB at 2048² where 2.16 GB was asked
/// for, with the old array still alive during the copy, and `ensure_slots` never
/// shrinks so it is permanent for the session. **A legal document with no
/// effects in it reaches this**: 64 layers each with a mask is 128 slices and
/// `begin_float` then asks for the 129th.
///
/// Restoring a 129 clamp was the obvious repair and is wrong — it breaks the
/// moment an effect claims a slice. A budget in bytes does not depend on a
/// ceiling that has already moved twice.
///
/// **Growing to `needed` exactly was the first repair and it is wrong, because
/// what it costs is O(N) *allocations* rather than O(N) copies.** Measured, a
/// 2048² document going from 16 slices to 128 one layer at a time — which is
/// just somebody putting a mask on each of 64 layers — is 112 growths and
/// 134 GB copied. The copying is fine, 1.2 GB a click at device bandwidth. The
/// 112 separate requests for a fresh multi-gigabyte texture with the old one
/// still live are not: a `create_texture` failure there is an uncaptured device
/// error, and therefore fatal. Rounding to a quantum makes the same document
/// **7 growths and 7.5 GB**, which is the whole justification — it is a count,
/// not a size.
///
/// **The peak transient argues the other way in the headline case, and that is
/// worth saying rather than leaving to be recomputed.** Over the 16-to-128
/// walk the quantum peaks at 4.03 GB against exact growth's 4.28. But for the
/// 129th slice alone it is 128 + 144 = **4.56 GB**, where exact growth and the
/// old accidental clamp both peak at 128 + 129 = 4.31 GB. So the rule chosen on
/// the strength of a fatal allocation failure raises the largest single
/// allocation, by 251 MB, in the one case this function is named for. The count
/// is what justifies it; the peak does not, and a reader who checks will find
/// that out.
///
/// **So the 129th slice of a 2048² document lands at 144, and that is not a
/// slip.** The waste is 15 slices — one quantum less one, 251,658,240 bytes —
/// which is the budget doing exactly what a budget is for. Landing on 129 was
/// the *accident* this whole function exists to explain: a ceiling that
/// happened to sit one above a power of two, so `.min` trimmed the overshoot to
/// nothing and nobody noticed the overshoot. Preserving an accident is not a
/// property.
///
/// **What the property is, stated so it survives its own test suite:** past the
/// budget, an array grows by whole quanta, so the overshoot is bounded by the
/// budget rather than by the size of the array. It is *not* "a legal document
/// must not double its array to reach one more slice", which this said and
/// which its own sibling test disproves — the first quantum step **is** a
/// doubling, `grown_capacity(16, 17, …)` at 2048² being exactly 16 to 32, and
/// by construction so is the step from `q` to `2q` on every canvas. The
/// difference from what was wrong before is the bound, not the ratio: 256
/// slices for 129 is unbounded overshoot, 32 for 17 is 240 MiB.
///
/// The trade, both directions, so it stays settled: at most one budget held,
/// against 105 allocations not made.
fn grown_capacity(current: u32, needed: u32, slice_bytes: u64) -> u32 {
    let mut capacity = current.max(1);
    while capacity < needed
        && u64::from(capacity)
            .saturating_mul(2)
            .saturating_mul(slice_bytes)
            <= GROWTH_DOUBLING_BUDGET_BYTES
    {
        capacity = capacity.saturating_mul(2);
    }
    if capacity >= needed {
        return capacity;
    }
    // Past the budget, up to the next whole quantum rather than to `needed`
    // exactly — see [`growth_quantum`] and the note above on what exact growth
    // costs in *allocations*.
    //
    // No `.max(needed)` after this: `div_ceil(q) * q >= needed` for every
    // `q >= 1`, and `saturating_mul` caps at `u32::MAX`, which is also `>=`.
    // One was here as belt and braces and came out — no mutation could reach
    // it, which by this file's own rule makes it a guard whose comment would
    // have claimed more than it demonstrated.
    // `the_growth_rule_always_reaches_what_was_asked_for` is the real cover.
    let quantum = growth_quantum(slice_bytes);
    needed.div_ceil(quantum).saturating_mul(quantum)
}

/// How many slices to round up to once doubling has stopped: as many as fit in
/// [`GROWTH_DOUBLING_BUDGET_BYTES`], and **never fewer than one**.
///
/// The same budget the doubling answers to, so there is one figure to reason
/// about rather than two. It is what keeps the number of *allocations* down
/// once each one is a multi-gigabyte texture: 16 slices at 2048², 4 at 4096²,
/// and **1 at 10000²**, where a single slice is already 400 MB and rounding up
/// would be exactly the speculation the budget refuses. That degeneration is
/// why this is the right shape rather than a compromise — the rule becomes
/// exact growth precisely where waste is unaffordable.
///
/// Zero is the failure to avoid, because `div_ceil` by it panics and a quantum
/// of zero has no meaning. It cannot happen: the division saturates to one for
/// any slice larger than the budget, and a `slice_bytes` of zero — a degenerate
/// canvas — never reaches here at all, since the doubling loop's condition is
/// then always true and it exits by satisfying `needed`.
fn growth_quantum(slice_bytes: u64) -> u32 {
    GROWTH_DOUBLING_BUDGET_BYTES
        .checked_div(slice_bytes)
        .unwrap_or(u64::from(u32::MAX))
        .clamp(1, u64::from(u32::MAX)) as u32
}

const DAB_STRIDE: u64 = std::mem::size_of::<Dab>() as u64;

/// The composite pass, with the blend modes in front of it.
///
/// The two shaders that combine premultiplied colours share one statement of
/// what each mode *is*, by being compiled from one text. CLAUDE.md's rule that
/// `composite.wgsl` and `commit.wgsl` must implement identical blending maths
/// is a rule about a preview and the thing that replaces it; a shared function
/// makes it structural where two hand-written copies of Multiply would leave it
/// to discipline. See `shaders/blend.wgsl`.
///
/// `concat!` rather than a runtime `format!`: this is a `&'static str` compiled
/// into the binary exactly as a lone `include_str!` was.
///
/// **A shader error's line number is not the file's.** naga sees one text, so
/// it counts from the first line of `blend.wgsl` and everything it reports
/// against `composite.wgsl` or `commit.wgsl` is shifted by that file's length.
/// Subtract it before going to look, or the line named will be plausible and
/// wrong — which is worse than one that is obviously out of range.
const BLEND_PRELUDE_COMPOSITE: &str = concat!(
    include_str!("../shaders/blend.wgsl"),
    include_str!("../shaders/composite.wgsl"),
);

/// The commit pass, with the same blend modes in front of it.
///
/// The other half of [`BLEND_PRELUDE_COMPOSITE`]: the preview and the thing
/// that replaces it are compiled from one copy of `blend.wgsl`.
const BLEND_PRELUDE_COMMIT: &str = concat!(
    include_str!("../shaders/blend.wgsl"),
    include_str!("../shaders/commit.wgsl"),
);

/// Per-dab colour, for a smudging stroke only.
///
/// `Rgba16Float` rather than `Rgba8Unorm` because these are **linear** values.
/// Eight bits of linear light bands visibly in the shadows, and a blender
/// working over a dark painting is precisely where it would show. Allocated
/// only when a smudging stroke starts, so an ordinary session never holds it.
const STROKE_COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// A **coloured stamp**'s colour: the tip's own pixels, for the tips that carry
/// any.
///
/// The same convention as [`LAYER_FORMAT`] and chosen for its reasons — sRGB
/// storage, alpha premultiplied in linear light. Eight bits are enough because
/// this *is* eight-bit source data: a `.gbr`, a `.gpb`'s pattern and a PNG in
/// the library all hold a byte a channel, so a wider texture would be storing
/// the same numbers in more space. That is the opposite of
/// [`STROKE_COLOR_FORMAT`], which holds values the engine computed rather than
/// values a file stated.
///
/// sRGB rather than `Rgba8Unorm` for the sake of the *hardware* decode: a
/// bilinear tap on an sRGB texture filters the decoded linear values, which is
/// what the dab pass needs, and encoding the low end costs nothing here where a
/// linear byte would band.
///
/// A coloured tip therefore costs five bytes a texel — one of coverage and four
/// of this — where a mask costs one. It is paid only by the brushes that carry a
/// colour, and at [`TipMask::MAX_SIZE`] it is 20 MB for a stamp nothing else in
/// this codebase would allocate at all.
const TIP_COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// The coverage attachment's blend state.
///
/// `Max` is the whole trick: coverage saturates instead of accumulating, so a
/// stroke crossing itself stays even. Shared by both non-building dab
/// pipelines, so smudging cannot quietly reintroduce the compounding this
/// prevents.
const COVERAGE_TARGET: wgpu::ColorTargetState = wgpu::ColorTargetState {
    format: STROKE_FORMAT,
    blend: Some(wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Max,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Max,
        },
    }),
    write_mask: wgpu::ColorWrites::ALL,
};

/// The coverage attachment's blend state for a **building-up** brush:
/// `a = cov + a(1 - cov)`, which is what one dab compositing over the last
/// means.
///
/// Expressed as `src * One + dst * (1 - src)`. `OneMinusSrc` reads the source
/// *colour*, and coverage lives in the red channel of a single-channel target,
/// so the factor is `1 - cov` per channel — exactly the complement wanted. The
/// alternative, writing coverage into the fragment's alpha as well and using
/// `OneMinusSrcAlpha`, would mean the two paths ran different shader code, and
/// the paint-versus-erase note in `CLAUDE.md` records what that costs: a
/// difference of blending has to live in the blend state or it drifts.
///
/// Nothing downstream changes. The result is still coverage in `0..1` in the
/// same texture, so `composite.wgsl` and `commit.wgsl` are untouched and stroke
/// opacity is still applied exactly once, at commit.
///
/// The floor worth knowing about: the scratch is `R8Unorm`, so a dab weaker
/// than about `1/255` rounds away and a stroke of them never builds, and one
/// only a little above it stalls partway — an increment of `cov * (1 - a)` that
/// falls below half a level stops moving the accumulator at all.
///
/// This is the one place a wider scratch would buy anything, and it was
/// measured rather than argued. Against exact arithmetic, over the whole of
/// Raghukamath's `pack01-drybrush` (stamped along a stroke at its own spacing,
/// 50 dabs deep), `R8Unorm` is at most **3 levels of 255** out and 2.8% of the
/// stroke's pixels are more than one level out. That preset no longer ships —
/// it asks for a paper texture Umber does not carry, see `docs/brushes.md` —
/// so it is now a brush the *importer* produces rather than one in the
/// library; it is the same stamp either way, and Umber's own "Stipple chalk"
/// is the shipped brush that takes this path. The
/// pathological case needs a *constant* faint coverage on one pixel for a
/// hundred dabs, which a bitmap tip cannot produce: the mask slides under the
/// stroke, so a pixel sees a different texel every dab. The mask is itself
/// `R8Unorm` too — filtering can interpolate below `1/255` at the edge of an
/// inked texel, but no *stored* value is that faint, so there is nothing for a
/// wider scratch to recover.
///
/// The other half of the answer is that `R16Unorm` is not available: it needs
/// `Features::TEXTURE_FORMAT_16BIT_NORM`, which Umber does not request, and
/// even with it wgpu guarantees only `storage` usage — not `RENDER_ATTACHMENT`.
/// `R16Float` is the only candidate that is a guaranteed blendable target on
/// `Features::empty()`. See the pressure note in CLAUDE.md.
const COVERAGE_BUILDUP_TARGET: wgpu::ColorTargetState = wgpu::ColorTargetState {
    format: STROKE_FORMAT,
    blend: Some(wgpu::BlendState {
        color: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrc,
            operation: wgpu::BlendOperation::Add,
        },
        alpha: wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::One,
            dst_factor: wgpu::BlendFactor::OneMinusSrc,
            operation: wgpu::BlendOperation::Add,
        },
    }),
    write_mask: wgpu::ColorWrites::ALL,
};

/// The colour attachment's blend state: premultiplied `over`.
///
/// Deliberately *not* `Max`, which is meaningless for colour — it would take
/// the brightest channel wherever a stroke overlapped itself. `over` makes each
/// pixel hold the most recent dabs' colour, which is what produces a smear that
/// trails along the stroke instead of one flat average of everything picked up.
const STROKE_COLOR_TARGET: wgpu::ColorTargetState = wgpu::ColorTargetState {
    format: STROKE_COLOR_FORMAT,
    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
    write_mask: wgpu::ColorWrites::ALL,
};

/// Instance layout, shared by both dab pipelines so they cannot disagree about
/// what a `Dab` looks like in memory.
const DAB_ATTRS: [wgpu::VertexAttribute; 7] = [
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x2,
        offset: 0,
        shader_location: 0,
    },
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32,
        offset: 8,
        shader_location: 1,
    },
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32,
        offset: 12,
        shader_location: 2,
    },
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32,
        offset: 16,
        shader_location: 3,
    },
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32x3,
        offset: 20,
        shader_location: 4,
    },
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32,
        offset: 32,
        shader_location: 5,
    },
    wgpu::VertexAttribute {
        format: wgpu::VertexFormat::Float32,
        offset: 36,
        shader_location: 6,
    },
];

const DAB_VERTEX_LAYOUT: &[wgpu::VertexBufferLayout] = &[wgpu::VertexBufferLayout {
    array_stride: DAB_STRIDE,
    step_mode: wgpu::VertexStepMode::Instance,
    attributes: &DAB_ATTRS,
}];

/// Side of the square the smudge probe composites into.
///
/// Small on purpose: it is averaged to a single colour, so it only has to be
/// wide enough that one stray pixel cannot dominate what the brush picks up.
const PROBE_SIZE: u32 = 8;
/// Format of every target that is not the window: PNG export, the eyedropper's
/// one pixel, and the smudge probe.
///
/// It must have a **pipeline of its own**. A render pipeline is compiled
/// against its target's format, and the surface is whatever the swapchain
/// offers — `Bgra8Unorm` on a good deal of Windows hardware. Compositing into
/// one of these with the screen's pipeline is a validation error that kills the
/// process, and it is invisible on any machine whose surface happens to be
/// `Rgba8Unorm`.
const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

const PROBE_FORMAT: wgpu::TextureFormat = OFFSCREEN_FORMAT;

/// Side of a layer thumbnail's target, in texels. See
/// [`umber_core::thumbnail`], which owns the number and the reason for it.
const THUMB_SIZE: u32 = umber_core::thumbnail::SIZE;
/// Bytes per row of a thumbnail readback. `THUMB_SIZE` RGBA texels is exactly
/// the 256-byte copy alignment, so there is no padding to stride over — which
/// is one of the two reasons that size was chosen.
const THUMB_ROW_BYTES: u32 = THUMB_SIZE * 4;
const _: () = assert!(THUMB_ROW_BYTES.is_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT));
/// Rows in a texture-to-buffer copy must be a multiple of 256 bytes, and eight
/// RGBA pixels are 32 — so each row is padded and the reader strides over it.
const PROBE_ROW_BYTES: u32 = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
/// Probes in flight at once. Two is enough to keep a sample arriving every
/// frame while never waiting on one: while the first is being mapped the second
/// is being rendered.
const PROBE_SLOTS: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ProbeState {
    /// Free to be handed to a new probe.
    Idle,
    /// A copy has been recorded into it but not yet mapped.
    Rendering,
    /// `map_async` is outstanding.
    Mapping,
}

/// `map_async` has not called back yet.
const PROBE_PENDING: u8 = 0;
/// The buffer is mapped and holds a sample.
const PROBE_MAPPED: u8 = 1;
/// The map failed. Nothing to read, and nothing to unmap either.
const PROBE_FAILED: u8 = 2;

/// One slot in the smudge probe's rotation: a staging buffer and where it is
/// up to.
struct Probe {
    buffer: wgpu::Buffer,
    state: ProbeState,
    /// One of the `PROBE_*` constants, written by the map callback — which runs
    /// on whichever thread polls the device, hence the atomic. Tri-state rather
    /// than a flag because a *failed* map leaves the buffer unmapped, and
    /// unmapping it anyway is as wrong as never returning the slot to service.
    outcome: Arc<AtomicU8>,
    /// The stroke this sample was taken for has ended, so whatever comes back
    /// must be thrown away rather than smeared into the next stroke.
    stale: bool,
}

/// Average a probe readback into one linear RGBA.
///
/// The composite's export path writes **sRGB** with straight alpha, so the
/// decode happens here. Averaging the sRGB bytes directly would be the classic
/// mistake — the mean of two gamma-encoded values is not the gamma encoding of
/// their mean, and a blender working across an edge would pick up a colour
/// lighter than either side of it.
///
/// Colour is weighted by coverage so that transparent pixels do not drag the
/// average towards whatever happens to sit in their unused colour channels.
fn average_probe(bytes: &[u8]) -> [f32; 4] {
    let decode = |b: u8| {
        let c = b as f32 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };

    let mut rgb = [0.0f32; 3];
    let mut alpha = 0.0f32;
    let mut weight = 0.0f32;
    for y in 0..PROBE_SIZE {
        let row = (y * PROBE_ROW_BYTES) as usize;
        for x in 0..PROBE_SIZE {
            let i = row + x as usize * 4;
            let Some(px) = bytes.get(i..i + 4) else {
                continue;
            };
            let a = px[3] as f32 / 255.0;
            alpha += a;
            weight += a;
            for c in 0..3 {
                rgb[c] += decode(px[c]) * a;
            }
        }
    }

    let n = (PROBE_SIZE * PROBE_SIZE) as f32;
    if weight <= 0.0 {
        return [0.0, 0.0, 0.0, 0.0];
    }
    [rgb[0] / weight, rgb[1] / weight, rgb[2] / weight, alpha / n]
}

// --- the whole-document capture --------------------------------------------

/// How much of a mapped capture buffer is read per frame.
///
/// Reading a mapped staging buffer reads uncached memory: a whole 16 MB layer
/// measured about 5 ms, which is a third of a 60 Hz frame spent on something
/// the user did not ask for. Four megabytes is comfortably under a millisecond
/// and costs only a few more frames — see [`Capture::copy_chunk`].
const CAPTURE_CHUNK_BYTES: usize = 4 << 20;

/// Where a [`Capture`]'s one staging buffer has got to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum StepState {
    /// Free. Nothing is outstanding on the GPU, so the next copy can be
    /// recorded into it — or the whole capture abandoned.
    Waiting,
    /// A copy has been recorded but `map_async` has not been called.
    Rendering,
    /// `map_async` is outstanding.
    Mapping,
}

/// A whole document on its way to the CPU, one buffer at a time.
///
/// The blocking readback [`CanvasRenderer::read_layer_rect`] performs is
/// acceptable once, at pointer-up, on an explicit Save — and is exactly wrong
/// on a timer, which is what an autosave is. So this is the smudge probe's idea
/// at document scale: a copy is recorded into the frame's own encoder,
/// `map_async` is called after that frame is submitted, and the bytes are
/// collected on some later frame by a poll that never waits.
///
/// **One layer in flight at a time, through one reused buffer.** Recording
/// every copy at once would cost the same in CPU calls, and would be wrong
/// twice over: the GPU would move the whole document in a single frame — over a
/// hundred megabytes on a 2048² stack — and the maps would then come home
/// together, landing every one of those memcpys in one frame. Both are exactly
/// the hitch this exists to avoid. Serialised, a frame pays either a recording
/// (microseconds) or one layer's memcpy, and the staging cost is one buffer
/// rather than one per layer. The price is that the capture takes a couple of
/// frames per layer, which for a five-minute timer is no price at all.
/// How many rows of a `padded`-byte-wide copy fit in one staging buffer.
///
/// A document can be far larger than the biggest buffer the device will make.
/// `Limits::downlevel_defaults` caps `max_buffer_size` at 256 MB and
/// `using_resolution` raises only the texture dimensions, so a 10000² canvas —
/// perfectly paintable, 400 MB of RGBA — could be drawn on and then not read
/// back: `create_buffer` refuses the size, and a validation error aborts the
/// process. That is exactly what happened, on the undo capture at the end of
/// the first stroke.
///
/// Raising the limit instead would be the wrong fix twice over: it would break
/// the rule that a desktop build may not depend on what a mobile GPU refuses,
/// and 256 MB is a limit real hardware has. So every readback goes a band of
/// rows at a time, and this decides how tall a band is.
///
/// Returns at least one row. A single row wider than the whole limit would
/// still be refused, but that needs a canvas 67 million pixels across — far
/// beyond `max_texture_dimension_2d`, which is checked long before this.
fn band_rows(limit: u64, padded: u32, height: u32) -> u32 {
    let rows = limit / u64::from(padded).max(1);
    rows.min(u64::from(height)).max(1) as u32
}

struct Capture {
    size: UVec2,
    /// Bytes per row of the copy, rounded up to the copy alignment.
    padded: u32,
    /// Which texture-array slices to read, in the order the caller asked for.
    slots: Vec<u32>,
    /// The stack as the flattened preview should composite it.
    draws: Vec<LayerDraw>,
    /// The one staging buffer, allocated on the first step and reused.
    buffer: Option<wgpu::Buffer>,
    /// Rows per staging buffer, from [`band_rows`]. A whole layer where the
    /// device's limit allows it, which is every ordinary document; a large
    /// canvas is read a band at a time instead. Set on the first
    /// [`CanvasRenderer::drive_capture`], which is the first place with a
    /// device to ask.
    band: u32,
    /// The first row of the band in flight, within the step's layer.
    row: u32,
    /// One of the `PROBE_*` constants, for the reason [`Probe::outcome`] is.
    outcome: Arc<AtomicU8>,
    state: StepState,
    /// The step in flight, or the next to be recorded. `slots.len()` is the
    /// flattened preview, which goes last.
    step: usize,
    /// One entry per step, filled in as each comes home.
    results: Vec<Option<Vec<u8>>>,
    /// The step in flight, as far as it has been copied out of the mapped
    /// buffer. See [`Capture::copy_chunk`].
    partial: Option<Vec<u8>>,
    /// The offscreen target the preview is drawn into, held until its copy has
    /// been submitted.
    merged_target: Option<wgpu::Texture>,
    /// The document has gone, or changed shape, so whatever comes home is
    /// worthless. The buffer stays where it is until its map has settled — see
    /// [`CanvasRenderer::reset_probes`] for why handing one back early is a
    /// crash rather than an untidiness.
    abandoned: bool,
    /// A map failed, so nothing can be assembled. The job is dropped once the
    /// buffer has settled.
    failed: bool,
}

impl Capture {
    /// One per layer, plus the flattened preview.
    fn steps(&self) -> usize {
        self.slots.len() + 1
    }

    /// True once every step has its bytes.
    fn complete(&self) -> bool {
        self.step >= self.steps()
    }

    /// True once nothing is outstanding on the GPU, so the job can be dropped.
    fn settled(&self) -> bool {
        self.state == StepState::Waiting
    }

    /// The rows the band in flight covers, as `[first, last)`.
    fn band_span(&self) -> (usize, usize) {
        let first = self.row as usize;
        let last = (first + self.band.max(1) as usize).min(self.size.y as usize);
        (first, last)
    }

    /// True once every band of this step has been copied out.
    fn step_done(&self) -> bool {
        self.partial.as_ref().map(Vec::len).unwrap_or(0)
            >= (self.size.x * 4) as usize * self.size.y as usize
    }

    /// Take the next slice of rows out of the mapped buffer. Returns true once
    /// the band in flight is out of it.
    ///
    /// Bounded because reading a mapped staging buffer reads *uncached* memory
    /// — a 16 MB layer measured about 5 ms on a mid-range discrete card, which
    /// is a third of a frame at 60 Hz for something the user did not ask for.
    /// Split into [`CAPTURE_CHUNK_BYTES`] pieces it is under a millisecond, and
    /// the capture merely takes a few more frames. On a five-minute timer that
    /// is not a cost at all.
    ///
    /// By rows rather than by bytes, because the copy's rows are padded to the
    /// alignment: chunking by rows makes the padding fall out for free.
    fn copy_chunk(&mut self) -> bool {
        let row = (self.size.x * 4) as usize;
        let height = self.size.y as usize;
        let (band_first, band_last) = self.band_span();
        let buffer = self.buffer.as_ref().expect("a mapped step has its buffer");
        // Only what the band actually wrote. The buffer is a full band long and
        // the last band is usually short, so mapping all of it would read the
        // previous band's rows back out of the tail.
        let mapped = buffer
            .slice(..(self.padded as u64) * ((band_last - band_first) as u64))
            .get_mapped_range();

        let out = self
            .partial
            .get_or_insert_with(|| Vec::with_capacity(row * height));
        // Absolute, because `partial` accumulates across every band of the step.
        let from = out.len() / row;
        let rows = (CAPTURE_CHUNK_BYTES / self.padded as usize).max(1);
        let to = (from + rows).min(band_last);
        for y in from..to {
            // Band-relative: row `band_first` of the layer is row 0 of the
            // buffer.
            let start = (y - band_first) * self.padded as usize;
            out.extend_from_slice(&mapped[start..start + row]);
        }
        to >= band_last
    }
}

/// Everything a document needs written down, read back without a stall.
///
/// `layers` are in **layer-texture form** — sRGB with alpha premultiplied in
/// linear space — which is what `umber_core::docformat::SaveLayer::pixels`
/// wants. `merged` is straight-alpha sRGB, as `SaveDocument::merged` wants.
/// Both come from the same passes the screen uses, so an autosaved file cannot
/// disagree with what was on screen.
pub struct DocumentCapture {
    pub size: UVec2,
    /// One buffer per slot asked for, in that order.
    pub layers: Vec<Vec<u8>>,
    pub merged: Vec<u8>,
}

// --- layer thumbnails -------------------------------------------------------

/// Which of a thumbnail's two passes is in flight.
///
/// They are the *same* pass with `reduce` flipped — see `thumbnail.wgsl`. The
/// second cannot be recorded until the first has come home, because what it
/// draws is decided by what the first found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ThumbPhase {
    /// Reducing the whole slice to the greatest alpha per cell, to find where
    /// the layer's content is.
    Bounds,
    /// Reducing that region to a mean, which is the picture.
    Picture,
}

/// One layer thumbnail on its way to the CPU.
///
/// Built like the smudge probe rather than like the autosave's capture: two
/// small copies, a `map_async` after the frame that recorded each, and a
/// collection on some later frame by a poll that never waits. Nothing here may
/// block, because the layer list is redrawn every frame and the blocking
/// readbacks — `read_layer_rect` and `read_layer_pieces` — are explicitly
/// reserved for a Save and for a pointer-up.
///
/// One at a time, for the reason [`Capture`] is one at a time: the cost is a
/// couple of frames of latency on something nobody is waiting for, and the
/// alternative is one staging buffer per layer.
struct ThumbJob {
    slot: u32,
    /// [`CanvasRenderer::slot_revision`] when the job began. Handed back with
    /// the picture so the caller can tell a thumbnail of the layer as it is now
    /// from one of the layer as it was two strokes ago.
    revision: u64,
    phase: ThumbPhase,
    /// The region the picture pass draws, from `umber_core::thumbnail::framed`.
    /// `None` until the bounds pass has come home.
    region: Option<umber_core::Rect>,
    state: StepState,
    /// One of the `PROBE_*` constants, for the reason [`Probe::outcome`] is.
    outcome: Arc<AtomicU8>,
    /// The layer was written to, or the document has gone, so whatever comes
    /// home describes a picture that is no longer there. Marked rather than
    /// dropped, exactly as a probe is — a buffer awaiting a map is still the
    /// GPU's, and handing it back early is a validation error and therefore an
    /// abort. See [`CanvasRenderer::reset_probes`].
    abandoned: bool,
}

/// A layer thumbnail, as the interface wants it.
pub struct Thumbnail {
    pub slot: u32,
    /// The revision the job began at. A caller holding a newer one knows to ask
    /// again rather than to draw this.
    pub revision: u64,
    /// Straight-alpha sRGB, `SIZE` square — or empty where the layer holds
    /// nothing at all, which is a state the list draws rather than a failure.
    pub rgba: Vec<u8>,
}

impl Thumbnail {
    /// True where the layer had no non-transparent pixel to show.
    pub fn is_empty(&self) -> bool {
        self.rgba.is_empty()
    }
}

/// One layer's contribution to the composite, in stack order.
#[derive(Clone, Copy, Debug, Default)]
pub struct LayerDraw {
    /// Texture-array slice holding the pixels.
    pub slot: u32,
    pub opacity: f32,
    /// Matches `umber_core::BlendMode::index`.
    pub blend: u32,
    pub visible: bool,
    /// Slice holding this layer's mask, when it has one. Another slice of the
    /// same array — see `umber_core::layer`'s module docs for why.
    ///
    /// `None` is the exact identity in the shader: the mask factor is 1.0 and
    /// nothing is sampled that matters.
    pub mask: Option<u32>,
    /// Bounded by the alpha of the nearest unclipped layer below.
    pub clipped: bool,
}

/// One layer's draw and the effects derived from it.
///
/// What [`CanvasRenderer::bake_effects`] takes, and it is deliberately a
/// *description* rather than a draw list with the effects already in it: which
/// slice each effect gets, which are refused for want of one, and where the
/// active layer ends up once they are spliced in are all this crate's to decide,
/// because [`MAX_EFFECT_SLICES`] is this crate's number. The caller supplies
/// what the document says; the renderer answers with what can be drawn.
#[derive(Clone, Copy, Debug)]
pub struct LayerEffects<'a> {
    pub draw: LayerDraw,
    /// Already in composite order, bottom to top — `Layer::effects` maintains
    /// that. Disabled ones and ones that would mark nothing are filtered here
    /// rather than by the caller, so one rule decides it: see
    /// [`effect_marks_nothing`].
    pub effects: &'a [Effect],
}

/// What the frame the bake is part of is doing.
#[derive(Clone, Copy, Debug)]
pub struct EffectFrame {
    /// Position in the caller's **plain** draw list of the layer receiving the
    /// stroke in flight, or `u32::MAX` for none. [`BakedStack::active_index`] is
    /// the same layer's position once effect draws are spliced in, and the
    /// composite has to be given *that* one.
    pub active_index: u32,
    /// The stroke in flight, needed because an effect on the layer being painted
    /// is baked from the layer **plus the scratch**.
    pub stroke: StrokeStyle,
    /// A stroke really is in flight. Distinct from `active_index` being valid,
    /// which is true whenever a layer is selected — and it is what makes the
    /// *end* of a stroke invalidate the bake even when the stroke was cancelled
    /// rather than committed, since a cancel writes no pixels and moves no slice
    /// revision.
    pub stroke_live: bool,
}

/// What [`CanvasRenderer::bake_effects`] produced.
pub struct BakedStack {
    /// Bottom to top, effect draws spliced in around the layers they belong to,
    /// in `docs/layer-effects.md` §4's order. This is what the composite takes.
    pub draws: Vec<LayerDraw>,
    /// Effects the document holds and this bake could not draw, because there was
    /// no slice or no room in the pass budget for them. Non-zero says the document
    /// is over budget, which is a thing to say out loud rather than a thing to be
    /// quiet about — see [`CanvasRenderer::effects_dropped`].
    pub dropped: usize,
    /// Where [`EffectFrame::active_index`]'s layer ended up, once effect draws are
    /// spliced in around it. `u32::MAX` stays `u32::MAX`, which the shader's
    /// `i == active_index` never matches.
    ///
    /// The composite has to be given **this** and not the plain index, or the
    /// stroke in flight previews on whichever draw happens to sit at the old
    /// number — an effect, or the wrong layer — and jumps into place at
    /// pointer-up.
    pub active_index: u32,
}

/// Would this effect put nothing on the canvas at all?
///
/// **The one rule for it, in this crate rather than in the caller**, so the draw
/// list and the bake cannot disagree about whether an effect exists. Two of the
/// three clauses are ordinary; the third is a decision.
///
/// * A disabled effect, or one at zero opacity, draws nothing. Obvious.
/// * An outline with no width has no band to trace. Obvious.
/// * **A drop shadow with no softness, no spread and no displacement draws
///   nothing**, and that is a choice rather than arithmetic. Such a shadow is
///   its own layer's shape sitting exactly underneath its own layer, and
///   §3.3's knockout multiplies it by `1 - coverage` — so all that could survive
///   is a rim at the layer's antialiased edge, at `c(1 - c)`, which is a mark
///   nobody asked for and is at most a quarter of the effect's colour. Declining
///   to draw it is what makes "a shadow of radius 0 is the exact identity" true
///   as *identity* rather than as "nearly". Photoshop draws the rim.
pub fn effect_marks_nothing(effect: &Effect) -> bool {
    if !effect.enabled || effect.opacity <= 0.0 {
        return true;
    }
    match effect.kind {
        EffectKind::Outline => effect.spread <= 0.0,
        EffectKind::DropShadow => {
            effect.softness <= 0.0 && effect.spread <= 0.0 && effect.distance <= 0.0
        }
    }
}

/// How the stroke in the scratch surface should look.
///
/// Preview and commit must be handed the *same* style — they implement the same
/// blending maths, and any disagreement shows up as the stroke jumping at
/// pointer-up. Passing them together makes that hard to get wrong.
#[derive(Clone, Copy, Debug)]
pub struct StrokeStyle {
    pub color: Color,
    /// Applied once, on commit — never folded into per-dab coverage.
    pub opacity: f32,
    pub mode: BrushMode,
    /// How the finished stroke combines with the layer it lands on:
    /// `umber_core::Brush::blend`, snapshotted with everything else here.
    ///
    /// It lives in this struct for the reason everything else in it does. The
    /// preview computes it in `composite.wgsl` and the commit computes it in
    /// `commit.wgsl`, out of one shared `composite_over` — so the two cannot
    /// disagree about what Multiply is, but they still have to be told the same
    /// mode, and being handed one `StrokeStyle` is what guarantees that.
    ///
    /// [`BlendMode::Normal`] is the path every stroke took before brushes had
    /// one, and it stays exactly as it was: the fixed-function blender does it,
    /// with no backdrop copy and no extra pass.
    ///
    /// Ignored when [`StrokeStyle::mode`] is [`BrushMode::Erase`] — an eraser
    /// deposits no colour for a mode to combine — and
    /// `umber_core::Brush::blend_applies` is where that is decided, so nothing
    /// here has to hold both in mind.
    pub blend: BlendMode,
    /// The stroke deposits a colour per dab — it smudges — so `color` is only
    /// the fallback and the real colour comes from the stroke's colour scratch.
    ///
    /// Must match what was passed to [`CanvasRenderer::draw_dabs`] for the same
    /// stroke. Preview and commit both read it, which is what keeps them from
    /// disagreeing about where the colour came from.
    pub per_dab_color: bool,
    /// The stroke is landing in the active layer's **mask** rather than in its
    /// pixels.
    ///
    /// The switch lives here, and only here, for the reason everything else in
    /// this struct does: preview and commit are handed the same `StrokeStyle`,
    /// so they cannot disagree about which of the two a stroke is going into.
    /// The slice it commits to is the one passed to
    /// [`CanvasRenderer::commit_stroke`] — a mask is an ordinary slice, so the
    /// commit pass needs no variant of its own; what this flag decides is where
    /// `composite.wgsl` blends the *preview*.
    pub on_mask: bool,
}

impl Default for StrokeStyle {
    fn default() -> Self {
        Self {
            color: Color::BLACK,
            opacity: 1.0,
            mode: BrushMode::Paint,
            blend: BlendMode::Normal,
            per_dab_color: false,
            on_mask: false,
        }
    }
}

/// How the dab pass should blend, for the whole of one stroke.
///
/// Both flags choose a *pipeline*, and a pipeline cannot be changed halfway
/// through a stroke without the dabs already in the scratch having been drawn
/// under the other rule. Hence one struct rather than two loose booleans:
/// whatever a stroke starts with, it finishes with.
///
/// Neither flag reaches [`CompositeParams`] or [`CanvasRenderer::commit_stroke`]
/// except through [`StrokeStyle::per_dab_color`], which they must agree with —
/// build-up is invisible downstream by design, because it changes only how
/// coverage arrives in the scratch and not what the scratch means.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DabStyle {
    /// Record a colour per dab as well as coverage. Must equal the
    /// [`StrokeStyle::per_dab_color`] handed to composite and commit.
    pub per_dab_color: bool,
    /// Accumulate coverage rather than taking a `max` of it. See
    /// [`COVERAGE_BUILDUP_TARGET`].
    pub build_up: bool,
}

impl DabStyle {
    /// Index into the pipeline matrix. Bit 0 is colour, bit 1 is build-up.
    fn index(self) -> usize {
        usize::from(self.per_dab_color) | usize::from(self.build_up) << 1
    }
}

/// The pixels a floating transform starts from, and where they sit.
///
/// One struct for both ways in, because everything after the first submission is
/// identical: a lift and a paste differ only in where the pixels come from and
/// whether the layer beneath keeps them.
pub struct FloatSource<'a> {
    /// The layer the float sits over. Its contents are the backdrop the float
    /// is previewed against, and a commit lands here.
    pub slot: u32,
    /// Where the floating pixels are before anything has been dragged, in
    /// document space.
    pub rect: PixelRect,
    /// Pixels to put down, in layer-texture form (sRGB, alpha premultiplied in
    /// linear space) and `rect`-sized. `None` **lifts** them out of `slot`
    /// instead, leaving a hole where they were.
    pub pixels: Option<&'a [u8]>,
    /// Clips both the lift and the hole it leaves. Ignored for a paste, which
    /// puts down exactly what it was given.
    pub mask: Option<&'a Selection>,
}

/// Where a floating transform's pixels have been dragged to.
#[derive(Clone, Copy, Debug)]
pub struct FloatParams {
    /// Destination document pixel back to where it came from — see
    /// `umber_core::transform`. The resampler walks the destination, so this is
    /// the direction the shader needs.
    pub inverse: Affine,
    /// The rectangle the result lands in, or `None` when the drag has carried
    /// it clean off the canvas. `None` is not "nothing to do": the previous
    /// destination still has to be restored.
    pub dest: Option<PixelRect>,
}

/// A floating region: pixels lifted out of a layer or pasted onto one, being
/// moved about before they are put down.
///
/// # How the preview cannot disagree with the commit
///
/// The stroke pipeline has two implementations of one blend — `composite.wgsl`
/// previews and `commit.wgsl` bakes — and CLAUDE.md is emphatic about keeping
/// them in step. This has none, and it is arranged that way rather than
/// disciplined that way:
///
/// * [`Float::base`] holds the layer as it will be *underneath* the float — the
///   original pixels, with the lifted region taken out. It is built once.
/// * The preview is `base` restored over the damaged rectangle, then the
///   transformed source drawn over it, into a spare slice of the layer array.
///   The composite pass is handed that slice **in place of the layer's own**,
///   so it composites a floating transform without knowing there is one: no new
///   uniform, no new branch, not a line of `composite.wgsl` touched.
/// * The commit is [`CanvasRenderer::render_float`] again, byte for byte, with
///   the layer's own slice as the target instead of the spare one.
///
/// So the preview and the committed result are not two renderings that have to
/// agree. They are the same two commands run twice, and the second one is the
/// first with a different destination.
struct Float {
    /// The layer this float sits over, and where a commit lands.
    layer_slot: u32,
    /// The layer-array slice the composite pass draws in place of
    /// [`Float::layer_slot`].
    preview_slot: u32,
    /// The layer with the lifted region removed. Canvas-sized.
    base: wgpu::Texture,
    /// The floating pixels at identity: canvas-sized, zero outside the region
    /// that was lifted or pasted. Held so it outlives the bind group.
    #[allow(dead_code)]
    source: wgpu::Texture,
    /// The selection's coverage, or a 1x1 placeholder. Snapshotted here rather
    /// than read off the renderer's stroke-path mask, so the two features
    /// cannot reach into each other. Held so it outlives the bind group.
    #[allow(dead_code)]
    mask: wgpu::Texture,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    /// Where the previous preview landed. The next one restores this as well as
    /// its own rectangle — without it the picture leaves a trail behind the
    /// drag.
    last_dest: Option<PixelRect>,
}

/// Where a smudging brush should sample the canvas, and what it is painting.
///
/// The stack and stroke are the same ones the screen composite is given, and
/// deliberately so: the probe reuses the composite pass, so a blender picks up
/// exactly what the painter can see under the brush — including the wet stroke.
#[derive(Clone, Copy)]
pub struct ProbeParams<'a> {
    /// Bottom-to-top, as [`CompositeParams::layers`].
    pub layers: &'a [LayerDraw],
    pub active_index: u32,
    pub stroke: StrokeStyle,
    /// Centre of the sample, in document pixels.
    pub doc_point: Vec2,
    /// Radius of the patch to average, in document pixels.
    pub radius: f32,
}

/// Everything the composite pass needs for a frame.
pub struct CompositeParams<'a> {
    pub camera: &'a Camera,
    /// Screen point the camera's centre sits on, in physical pixels. This is
    /// the middle of the *canvas region*, not the window — panels take a bite
    /// out of the window and the document should sit in what remains.
    pub pivot: Vec2,
    /// Bottom-to-top.
    pub layers: &'a [LayerDraw],
    /// Stack position (not slot) receiving the in-progress stroke.
    pub active_index: u32,
    pub stroke: StrokeStyle,
    /// Surround colour, display-space RGB.
    pub backdrop: [f32; 3],
    /// Render for file output rather than for the screen.
    pub export: bool,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct DabUniforms {
    doc_size: [f32; 2],
    /// The tip's proportions, longer side normalised to 1. `[1.0, 1.0]` with no
    /// tip, which is the exact identity in the shader.
    tip_scale: [f32; 2],
    /// Non-zero when a real tip texture is bound. Scalar padding, not a vec2 —
    /// see the uniform-layout note in CLAUDE.md.
    use_tip: u32,
    /// How hard the paper bites, 0..1. Zero is the exact identity.
    grain_strength: f32,
    /// Side of one grain tile in document pixels.
    grain_scale: f32,
    _pad: f32,
    /// Where the selection mask is mapped to, in document pixels.
    sel_min: [f32; 2],
    /// Its size. `[1.0, 1.0]` with no selection, so the shader's divide is
    /// never by zero even though its result is thrown away.
    sel_size: [f32; 2],
    /// Non-zero when a real mask is bound. Unlike the tip and the grain this
    /// cannot be folded into a placeholder — see the WGSL struct.
    use_selection: u32,
    /// Non-zero when the tip carries a colour of its own. Took the place of one
    /// of the three padding words already here, so the block is the size it
    /// always was.
    use_tip_color: u32,
    /// Scalar padding, not a vec3: see the uniform-layout note in CLAUDE.md.
    _pad2: f32,
    _pad3: f32,
}

impl DabUniforms {
    /// The uniforms for a document with no tip, no grain and no selection:
    /// every factor the shader multiplies by is one.
    fn plain(doc_size: UVec2) -> Self {
        Self {
            doc_size: [doc_size.x as f32, doc_size.y as f32],
            tip_scale: [1.0, 1.0],
            use_tip: 0,
            grain_strength: 0.0,
            grain_scale: 1.0,
            _pad: 0.0,
            sel_min: [0.0, 0.0],
            sel_size: [1.0, 1.0],
            use_selection: 0,
            use_tip_color: 0,
            _pad2: 0.0,
            _pad3: 0.0,
        }
    }
}

/// Mirrors `View` in `composite.wgsl`, byte for byte.
///
/// The arithmetic, because it is the one uniform here large enough for the
/// answer to be in doubt. Four `vec2<f32>` (32) + three `vec4<f32>` (48) +
/// eight scalars (32) = **112 bytes** of head, which is 16-aligned, so
/// `layers` starts there with no padding inserted. Each array is
/// `MAX_DRAWS × 16`, so the whole block is `112 + 2 × 191 × 16` = **6224
/// bytes**, against `downlevel_defaults`' `max_uniform_buffer_binding_size` of
/// 16 KiB. `the_view_uniform_fits_the_smallest_binding_a_device_must_offer`
/// measures all of it rather than trusting the sum written here.
///
/// It was 2160 bytes while the arrays held 64; the growth is the effect-draw
/// budget and is paid in bytes uploaded per frame, not per fragment.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ViewUniforms {
    scale: [f32; 2],
    offset: [f32; 2],
    doc_size: [f32; 2],
    pivot: [f32; 2],
    stroke_color: [f32; 4],
    backdrop: [f32; 4],
    /// Premultiplied linear; see the WGSL struct. `vec4` is 16-aligned on both
    /// sides and sits on a 16-byte boundary here, so this insertion moves
    /// nothing after it.
    background: [f32; 4],
    layer_count: u32,
    stroke_mode: u32,
    active_index: u32,
    checker: f32,
    is_export: u32,
    per_dab_color: u32,
    /// The stroke in flight is going into the active layer's mask.
    stroke_on_mask: u32,
    /// The brush's blend mode, in [`BlendMode::index`]'s numbering. Took the
    /// place of the padding word that was here, so the block is the size it
    /// always was — see the WGSL struct.
    stroke_blend: u32,
    /// (opacity, blend, slot, visible) per draw.
    layers: [[f32; 4]; MAX_DRAWS],
    /// (mask slot, has mask, clipped, unused) per draw. See the WGSL struct for
    /// why this is a second array rather than bits in the first.
    extra: [[f32; 4]; MAX_DRAWS],
}

/// Mirrors `Xf` in `transform.wgsl`. Every member is a `vec2<f32>`, which is
/// 8-aligned on both sides, so the packing is the obvious one — see the
/// uniform-layout note in CLAUDE.md for why a `mat2x2` here would not be.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct TransformUniforms {
    rect_min: [f32; 2],
    rect_max: [f32; 2],
    doc_size: [f32; 2],
    inv_x: [f32; 2],
    inv_y: [f32; 2],
    inv_t: [f32; 2],
    mask_min: [f32; 2],
    mask_size: [f32; 2],
    use_mask: u32,
    /// Scalar padding, not a vec3: see the uniform-layout note in CLAUDE.md.
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

/// Mirrors `Commit` in `commit.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CommitUniforms {
    rect_min: [f32; 2],
    rect_max: [f32; 2],
    doc_size: [f32; 2],
    _pad0: [f32; 2],
    color: [f32; 4],
    mode: u32,
    per_dab_color: u32,
    /// [`BlendMode::index`]. Took one of the two padding words already here, so
    /// the block is the size it always was. Scalars rather than a `vec3`: see
    /// the uniform-layout note in CLAUDE.md.
    blend: u32,
    _pad2: u32,
}

/// Mirrors `Flip` in `flip.wgsl`.
///
/// `vec2<u32>` is 8-aligned on both sides and the two scalars after it pack
/// into the same 16 bytes, so the block is 16 wide with no surprises. Scalar
/// padding, not a `vec3`: see the uniform-layout note in CLAUDE.md.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FlipUniforms {
    doc_size: [u32; 2],
    axis: u32,
    _pad: u32,
}

/// Mirrors `Thumb` in `thumbnail.wgsl`.
///
/// Every member is a `vec2` or a scalar, so both sides pack to 48 bytes with no
/// alignment surprise. Scalar padding, not a `vec2`: see the uniform-layout note
/// in CLAUDE.md.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ThumbUniforms {
    src_min: [f32; 2],
    src_size: [f32; 2],
    dest: [u32; 2],
    layer_size: [u32; 2],
    slot: u32,
    reduce: u32,
    _pad0: u32,
    _pad1: u32,
}

/// Mirrors `Cfg` in `effect.wgsl`, byte for byte.
///
/// The arithmetic, because there is no `vec3` here to make it interesting and
/// that is exactly why: one `vec4<f32>` (16) + four `vec2<f32>` (32) + fifteen
/// scalars (60) = 108, rounded up to the struct's 16-byte alignment = **112**.
/// Every `vec2` sits on an 8-byte boundary and the scalars are 4-aligned, so
/// both sides pack the obvious way. Padding is a scalar, not a vector — see the
/// uniform-layout note in CLAUDE.md.
///
/// One block per **pass**, reached with a dynamic offset, for the reason
/// [`CommitUniforms`] is: a bake records a dozen or more passes into one encoder
/// and `Queue::write_buffer` is flushed before the command buffer, so writing
/// one block repeatedly would leave every pass reading the last one's numbers.
#[repr(C)]
#[derive(Clone, Copy, Default, Pod, Zeroable)]
struct EffectUniforms {
    tint: [f32; 4],
    size: [f32; 2],
    src_size: [f32; 2],
    offset: [f32; 2],
    step: [f32; 2],
    radius: i32,
    down: i32,
    spread: f32,
    /// [`EffectShape`]'s discriminant.
    shape: u32,
    slot: i32,
    mask_slot: i32,
    has_mask: u32,
    stroke_here: u32,
    stroke_mode: u32,
    stroke_opacity: f32,
    stroke_on_mask: u32,
    stroke_gray: f32,
    grow: u32,
    k: i32,
    invert: u32,
    /// Remove the layer's own shape from the effect at resolve time. **A drop
    /// shadow and nothing else** — see `effect.wgsl`'s header, and
    /// [`EffectShape`].
    knockout: u32,
}

/// What shape the grow pass builds. Mirrors `effect.wgsl`'s `SHAPE_*`.
///
/// **This is where `docs/layer-effects.md` §3.3's correction lives.** That
/// section makes the knockout a property of a *side* of the stack — anything
/// compositing under the layer gets it — and cites Photoshop's control, whose
/// name is "Layer knocks out **drop shadow**". It is named for the drop shadow
/// because it is the drop shadow's, and generalising it made a centred outline
/// undrawable: a stroke sits *on* the edge, so removing it wherever the layer
/// covers deletes exactly the half somebody asked for.
///
/// So the confinement is per kind and position, chosen here, and only
/// [`EffectShape::Dilate`] pairs with a knockout at resolve time. What that buys
/// beyond a working Centre: an outline's confinement now happens *before* the
/// blur, so a soft stroke is soft on both sides where it used to be sheared flat
/// against the layer's own edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EffectShape {
    /// The coverage grown by the spread. A drop shadow, and the only shape whose
    /// confinement is the resolve's knockout.
    Dilate = 0,
    /// The outward band times `1 - coverage`, which is what "outside the edge"
    /// means rather than a knockout.
    Outer = 1,
    /// The inward band, whole. `LayerDraw::clipped` bounds it, and multiplying by
    /// the coverage here as well would apply the layer's alpha twice.
    Inner = 2,
    /// Both bands mixed by the coverage. Reads the outward band out of the plane
    /// a [`EffectShape::Raw`] pass wrote, which is why a centred outline is the
    /// one effect that floods **twice**.
    Centre = 3,
    /// A band with no confinement, the first half of a centred outline. Never an
    /// effect on its own.
    Raw = 4,
}

/// How many pass blocks the bake's uniform buffer holds.
///
/// One effect is an extract, one distance field or two, a downsample, four box
/// passes and a resolve — where a field is a seed, `bitlen(span) + 1` flood steps
/// and a grow. The flood is what makes that unbounded-looking, and it is bounded
/// by the canvas rather than by the spread; see [`CanvasRenderer::plan_field`].
/// **Two fields, because a centred outline floods twice**, which puts the worst at
/// 45 passes at 32768 square. [`EFFECT_MAX_PASSES_PER_EFFECT`] is where that is
/// derived and where the headroom is stated.
///
/// **The buffer is sized for `effect::MAX_ENABLED` of them, which the model will
/// really install**: 64 layers times two kinds is 128, and the cap is 127. At 512
/// blocks — five times too few — the whole bake was refused, `plain()` came back,
/// and `effects_dropped` reported **zero** while nothing drew, every frame, for
/// as long as the document was open. A panel reading that figure would have said
/// the document was within its budget while showing none of it, which is the
/// silent version of the failure §6.1a exists to prevent. The plan is now bounded
/// as well, so an overrun is *impossible* rather than merely unlikely, and an
/// effect that will not fit is dropped through the same visible path as one that
/// has no slice.
///
/// At the 256-byte alignment `downlevel_defaults` guarantees this is **1.49 MiB**
/// (48 × 127 blocks), and it is noise against the 16 KiB *binding* limit because
/// exactly one block is bound at a time — the buffer's own size is not a bound
/// anything checks. It is allocated once per document that has an effect, and
/// never on the drawing path.
const EFFECT_PASS_BLOCKS: u64 =
    EFFECT_MAX_PASSES_PER_EFFECT as u64 * umber_core::effect::MAX_ENABLED as u64;

/// The most passes one effect can ask for, which is what bounds the plan.
///
/// A **centred outline** is the worst, because it is the one effect that floods
/// twice: two seeds, two sets of `ceil(log2(span)) + 1` steps and two grows, plus
/// the extract, the downsample, four box passes and the resolve. Derived rather
/// than counted by hand — 45 at 32768 square and 47 at 65536, which is past every
/// `max_texture_dimension_2d` a device reports.
///
/// **The span is the canvas's longest side and not the downlevel limit**, which
/// is the reading that made the guard read 37: `Gpu::new` asks for
/// `using_resolution`, so `downlevel_defaults`' 2048 is what a canvas is
/// guaranteed to *reach* and says nothing about the largest one a device allows.
///
/// Forty-eight leaves three passes of headroom at 32768 and one at 65536, which
/// is thin — so **check this figure when a pass is added**, and note that
/// overrunning it is graceful rather than fatal: `bake_effects` keeps
/// `EFFECT_PASS_BLOCKS / EFFECT_MAX_PASSES_PER_EFFECT` effects and
/// `run_effect_steps` refuses a plan past the buffer, both of which are counted
/// in `dropped` and neither of which is a validation error.
///
/// `the_pass_budget_covers_the_effects_the_model_permits` derives the figure from
/// the planner's own arithmetic rather than restating it, which is what caught
/// the first draft at a fifth of what it needed — and then caught its own second
/// draft counting one field where the worst case floods twice.
const EFFECT_MAX_PASSES_PER_EFFECT: usize = 48;

/// Stride of one [`EffectUniforms`] block.
///
/// `min_uniform_buffer_offset_alignment` is 256 on every device
/// `downlevel_defaults` describes, and a dynamic offset must be a multiple of
/// it. Stated as a constant rather than read from the device because the buffer
/// is sized from it as well as indexed by it, and two spellings would be two
/// chances to disagree.
const EFFECT_BLOCK_STRIDE: u64 = 256;

const _: () = assert!(
    EFFECT_BLOCK_STRIDE >= std::mem::size_of::<EffectUniforms>() as u64,
    "an effect pass's uniform block does not fit its own stride"
);

/// Which pass of the bake a recorded step is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EffectPass {
    Extract,
    Seed,
    Flood,
    Grow,
    Down,
    Box,
    Resolve,
}

/// Where one recorded step writes.
#[derive(Clone, Copy, Debug)]
enum EffectTarget {
    Coverage,
    Grown,
    /// The outward band a centred outline's first flood produced, held while its
    /// second flood runs. See [`EffectShape::Centre`].
    Band,
    Blur(usize),
    Seed(usize),
    /// A slice of the layer array: the effect's own, and the only target of the
    /// whole bake that outlives it.
    Slice(u32),
}

/// One recorded pass, before anything is written or encoded.
///
/// The bake is planned whole and then run, rather than recorded as it is worked
/// out, because every pass reads its numbers out of one uniform buffer at
/// *submit* time — see [`EffectUniforms`]. Planning first is what lets all the
/// blocks be written in one `write_buffer` before the first pass is recorded.
struct EffectStep {
    pass: EffectPass,
    target: EffectTarget,
    /// Which of the scratch's bind groups this pass reads through.
    bind: usize,
    /// The region of the target the pass covers, in texels. Smaller than the
    /// attachment whenever the blur is running downsampled, which is what
    /// `set_viewport` is for.
    viewport: UVec2,
    cfg: EffectUniforms,
}

/// The canvas-sized working set every effect on this document shares.
///
/// **Shared rather than per effect**, which is what keeps the memory bounded by
/// the document rather than by how many shadows are switched on: one effect is
/// baked at a time, so the coverage, the grown shape, the tent's ping-pong pair
/// and the flood's seed pair are reused by all of them. Four `R8Unorm` planes
/// always, one more for a centred outline's band, and two `Rg16Uint` for the
/// flood: at 2048² that is 16 MB, 20 MB and 52 MB; at 10000² it is 400 MB, 500 MB
/// and 1,200 MB — the figure `docs/layer-effects.md` §3.4 records, and the reason
/// stage 3 bounds the bake by region.
///
/// The seed pair and the band are [`Option`] because most effects need neither: a
/// drop shadow with no spread is a blur of the coverage and nothing else, which is
/// the setting every application opens one at, and only a *centred* outline needs
/// two distance fields.
struct EffectScratch {
    size: UVec2,
    /// The layer's coverage after its mask and its wet stroke.
    coverage: wgpu::TextureView,
    /// That coverage grown by the effect's spread.
    grown: wgpu::TextureView,
    /// The tent's ping-pong pair, at full resolution so that the same pair
    /// serves a downsampled blur in its top-left corner and a full-resolution
    /// one whole. A pair sized to the downsample would have to be reallocated
    /// whenever a radius crossed [`EFFECT_FULL_RES_SOFTNESS`].
    blur: [wgpu::TextureView; 2],
    seeds: Option<[wgpu::TextureView; 2]>,
    /// Holds a centred outline's outward band while its inward flood runs.
    /// Lazily allocated like the seed pair and for the same reason: it is the one
    /// position that needs two fields, and a document with no centred outline
    /// should not pay 100 MB at 10000² for the possibility of one.
    band: Option<wgpu::TextureView>,
    /// Held so the views above outlive the bind groups that reference them.
    #[allow(dead_code)]
    textures: Vec<wgpu::Texture>,
    uniforms: wgpu::Buffer,
    /// The bind groups, in [`EffectBind`]'s order.
    binds: Vec<wgpu::BindGroup>,
    /// The layer array's capacity when [`EffectScratch::binds`] were built. The
    /// extract reads the array, so a growth that reallocated it leaves those
    /// bind groups naming a texture nothing draws into any more.
    bound_capacity: u32,
}

/// Which bind group a pass reads through. Indices into
/// [`EffectScratch::binds`].
///
/// Built once per scratch and reused by every effect, because the views they
/// name are fixed for the document's life. Only the uniform block changes per
/// pass, and that is a dynamic offset.
///
/// **There are sixteen rather than four because a colour attachment may not also
/// be bound**, and every one of these passes writes into the same small set of
/// textures the others read. That is the constraint `flip.wgsl` works around with
/// a scratch and `commit_blended` works around with a copy; here it is worked
/// around by binding a 1x1 stand-in wherever an entry point does not read a slot.
/// Two of them are not obvious and both were validation errors before they were
/// bind groups: the extract writes the coverage, so it must not bind it, and the
/// **resolve writes a slice of the layer array**, so it must not bind the array —
/// even though `fs_resolve` never reads it.
#[allow(clippy::enum_variant_names)]
enum EffectBind {
    /// The real layer array and the stroke scratch. The only pass that reads
    /// either, and the only one whose target is not in this list.
    Extract = 0,
    /// The coverage, and nothing else: the seed pass, and a grow with no spread.
    Coverage = 1,
    /// The coverage and one of the flood pair.
    Grow0 = 2,
    Grow1 = 3,
    /// One of the flood pair alone.
    Flood0 = 4,
    Flood1 = 5,
    /// One coverage field as `src`, for a blur pass.
    SrcGrown = 6,
    SrcBlur0 = 7,
    SrcBlur1 = 8,
    /// The same, plus the coverage the knockout needs.
    ResolveGrown = 9,
    ResolveBlur0 = 10,
    ResolveBlur1 = 11,
    /// The coverage itself as `src`, for an effect with no spread: there is
    /// nothing to grow, so the shape *is* the coverage and the grow pass is not
    /// recorded at all. Two bind groups rather than one because the blur reads the
    /// shape without the coverage beside it and the resolve reads both.
    SrcCoverage = 12,
    ResolveCoverage = 13,
    /// The band plane as `src`, plus the coverage and one of the flood pair: a
    /// centred outline's combining grow. See [`EffectShape::Centre`].
    CombineSeed0 = 14,
    CombineSeed1 = 15,
}

const EFFECT_BIND_COUNT: usize = 16;

/// One effect's slice, and what it was baked from.
///
/// **Keyed on the slot the *draw* carries, not on the layer**, which is
/// `docs/layer-effects.md` §5.2's rule and is what makes a floating transform
/// fall out rather than needing one: during a drag the draw carries the preview
/// slice, so the effect baked from it is a different cache entry from the one
/// baked from the layer's own slice, and the commit swaps back to an entry that
/// is stale for the ordinary reason.
struct CachedEffect {
    source: u32,
    mask: Option<u32>,
    kind: EffectKind,
    /// The effect slice this entry owns.
    slot: u32,
    /// The layer slice's revision when it was last baked.
    source_revision: u64,
    /// The mask slice's, or 0 where there is none.
    mask_revision: u64,
    /// A hash of every parameter the *pixels* depend on. Opacity and blend mode
    /// are deliberately absent: those are the draw's, applied by the composite,
    /// so dragging either slider costs no rebake.
    params: u64,
    /// Whether the last bake folded a live stroke in. A change either way is
    /// staleness, which is how a *cancelled* stroke invalidates the bake — a
    /// cancel writes no pixels, so no slice revision moves.
    live: bool,
}

/// Every effect's slice, and the scratch they are baked through.
///
/// Effect slices are handed out from `[base, base + capacity)` where `base` is
/// one past everything `LayerStack` has claimed — the `+ 1` being the slice a
/// floating transform previews into, which is taken at exactly
/// `slot_capacity_needed()`. That is above every *parked* slice as well as every
/// live one, because `SlotPool` compacts only its tail, and it is what makes
/// §4.2 safe: **an effect slice may be freed rather than parked**, because the
/// model can never hand it to a layer and so no `PixelPatch` can ever name it.
#[derive(Default)]
struct EffectCache {
    /// The lowest slice effects may use. A change means the stack claimed or
    /// released slices, so every entry is dropped and rebaked at its new number.
    base: u32,
    entries: Vec<CachedEffect>,
    /// Offsets from `base` nobody holds, ascending.
    free: Vec<u32>,
    /// One past the highest offset ever handed out.
    next: u32,
    scratch: Option<EffectScratch>,
    /// Effects the last bake could not give a slice to. Non-zero says the
    /// document is over its effect budget, which the panel is meant to say out
    /// loud — see [`CanvasRenderer::effects_dropped`].
    dropped: usize,
    /// How many bakes have run. Observation only, and the only way a test can
    /// say "this frame rebaked nothing".
    bakes: u64,
}

/// The layer texture array and the views onto it.
struct LayerStore {
    texture: wgpu::Texture,
    /// Sampled by the composite pass.
    array_view: wgpu::TextureView,
    /// One per slice, used as render targets by commit and clear.
    slot_views: Vec<wgpu::TextureView>,
    /// The same slices seen as [`LAYER_FORMAT_LINEAR`], for the flip pass and
    /// nothing else. Built here rather than per flip because a view is cheap to
    /// hold and the alternative is allocating one per slice per command.
    raw_slot_views: Vec<wgpu::TextureView>,
    capacity: u32,
}

impl LayerStore {
    fn new(device: &wgpu::Device, size: UVec2, capacity: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("umber-layers"),
            size: wgpu::Extent3d {
                width: size.x,
                height: size.y,
                depth_or_array_layers: capacity,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: LAYER_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            // The flip pass needs to see these bytes without the transfer
            // function on the way in or out — see [`LAYER_FORMAT_LINEAR`].
            // Declared here because a view of a format the texture was not
            // created for is a validation error, not a conversion.
            view_formats: &[LAYER_FORMAT_LINEAR],
        });

        let array_view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("umber-layers-array"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });

        let slot_views = (0..capacity)
            .map(|i| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("umber-layer-slot"),
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: i,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();

        let raw_slot_views = (0..capacity)
            .map(|i| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("umber-layer-slot-raw"),
                    format: Some(LAYER_FORMAT_LINEAR),
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: i,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();

        Self {
            texture,
            array_view,
            slot_views,
            raw_slot_views,
            capacity,
        }
    }
}

/// Everything that does not depend on the document: compiled shaders,
/// pipelines, bind group layouts and the sampler.
///
/// Split out so a second open document can have its own textures without its
/// own shaders. Every field is a reference-counted wgpu handle, so cloning this
/// is a few atomic increments where rebuilding it is three shader compilations
/// and four pipeline creations — a stall the user would pay on the frame they
/// open a document. See [`CanvasRenderer::for_document`].
#[derive(Clone)]
struct Shared {
    sampler: wgpu::Sampler,
    /// Repeats where [`Shared::sampler`] clamps. A paper tile has to wrap — it
    /// covers the whole document — and a tip stretched over its dab must not.
    grain_sampler: wgpu::Sampler,

    /// The four dab pipelines, indexed by [`DabStyle::index`].
    ///
    /// Two independent binary choices, so four — but written once and built by
    /// a loop, because they differ in exactly two fields and four copies of a
    /// pipeline descriptor is four places for the vertex layout to drift. The
    /// coloured pair carry a second attachment that nearly every stroke does
    /// not want; the building pair swap the coverage target's blend state. All
    /// four share one shader module and one pipeline layout.
    dab_pipelines: [wgpu::RenderPipeline; 4],
    dab_layout: wgpu::BindGroupLayout,

    composite_pipeline: wgpu::RenderPipeline,
    /// The same pass compiled for [`OFFSCREEN_FORMAT`], for export, the
    /// eyedropper and the smudge probe. See that constant for why the screen's
    /// pipeline cannot be reused.
    composite_offscreen_pipeline: wgpu::RenderPipeline,
    composite_layout: wgpu::BindGroupLayout,

    commit_layout: wgpu::BindGroupLayout,
    commit_pipeline: wgpu::RenderPipeline,
    commit_erase_pipeline: wgpu::RenderPipeline,
    /// A commit for a brush whose blend mode is not Normal.
    ///
    /// Its own layout because it needs a fifth binding — a copy of the layer
    /// under the piece, which the pass cannot sample out of its own attachment
    /// — and because its uniform is bound with a dynamic offset, one block per
    /// piece. See [`CanvasRenderer::commit_blended`].
    commit_blend_layout: wgpu::BindGroupLayout,
    commit_blend_pipeline: wgpu::RenderPipeline,

    /// Mirrors one layer slice. See `flip.wgsl` for why it is its own pass
    /// rather than a copy or a use of the transform resampler.
    flip_layout: wgpu::BindGroupLayout,
    flip_pipeline: wgpu::RenderPipeline,

    /// Reduces a rectangle of one slice to a 64-square, for the layer list.
    /// One pipeline for both of a thumbnail's passes — they differ by a uniform
    /// and nothing else. See `thumbnail.wgsl`.
    thumb_layout: wgpu::BindGroupLayout,
    thumb_pipeline: wgpu::RenderPipeline,

    /// Bakes a layer effect. See `effect.wgsl`.
    ///
    /// **One layout for all seven passes**, holding every binding any of them
    /// reads, so a pass is chosen by a pipeline and a uniform block and never by
    /// a second bind-group shape. That is what lets the bind groups be built once
    /// per document and reused by every effect — the alternative was a layout per
    /// pass and a bind group per pass per frame, which is allocation churn on the
    /// drawing path for nothing. Extra entries a given entry point does not read
    /// are legal; missing ones are not, which is the direction that matters.
    effect_layout: wgpu::BindGroupLayout,
    /// The layer's alpha, after its mask and after the wet stroke, into
    /// [`STROKE_FORMAT`].
    effect_extract: wgpu::RenderPipeline,
    effect_seed: wgpu::RenderPipeline,
    effect_step: wgpu::RenderPipeline,
    effect_grow: wgpu::RenderPipeline,
    effect_down: wgpu::RenderPipeline,
    effect_box: wgpu::RenderPipeline,
    /// The only one of the seven that writes a layer slice, and therefore the
    /// only one whose target is [`LAYER_FORMAT`] — an **sRGB** view, like a
    /// layer, so the composite decodes what it wrote. Every intermediate above
    /// it is `R8Unorm` and carries no transfer function at all.
    effect_resolve: wgpu::RenderPipeline,

    transform_layout: wgpu::BindGroupLayout,
    /// `dst * cov` — takes the selected pixels into the floating copy.
    transform_keep_pipeline: wgpu::RenderPipeline,
    /// `dst * (1 - cov)` — leaves the hole behind in the base.
    transform_take_pipeline: wgpu::RenderPipeline,
    /// The resampler: the floating copy through the inverse transform,
    /// premultiplied source-over.
    transform_draw_pipeline: wgpu::RenderPipeline,
}

pub struct CanvasRenderer {
    doc_size: UVec2,
    /// The document background, premultiplied linear.
    ///
    /// A field rather than a [`CompositeParams`] member because it belongs to
    /// the *document*, and a renderer already is one document's. That is not
    /// tidiness: `export_rgba`, `pick_colour` and `probe_canvas` all build
    /// their own `CompositeParams`, and a per-frame parameter would have to be
    /// threaded into each of them — three more places for the export to stop
    /// matching the screen. Held here, they cannot disagree.
    background: [f32; 4],
    shared: Shared,

    layers: LayerStore,
    #[allow(dead_code)]
    stroke: wgpu::Texture,
    stroke_view: wgpu::TextureView,
    /// Per-dab colour, or a 1x1 placeholder until a smudging stroke first needs
    /// it. Held so it outlives the bind groups referencing it.
    stroke_color: wgpu::Texture,
    stroke_color_view: wgpu::TextureView,
    has_stroke_color: bool,
    /// Staging buffers for the smudge probe, rotated so a stroke never waits on
    /// the GPU to tell it what colour it is passing over.
    probes: Vec<Probe>,
    /// The autosave's whole-document readback, if one is in flight. At most one
    /// per document: a second would double the staging cost for a job that is
    /// already going to be repeated in five minutes.
    capture: Option<Capture>,
    /// The largest staging buffer this device will create, in bytes.
    ///
    /// Taken from the device rather than assumed, and honoured by every
    /// readback here — see [`band_rows`] — **and now by the one write that can
    /// carry a whole canvas**, [`CanvasRenderer::write_layer_rect`]. Held as a
    /// field so a test can lower it and drive the banded path on a document
    /// small enough to check by hand; on a real device it would take a 8192²
    /// canvas to reach.
    readback_limit: u64,

    dab_bind_group: wgpu::BindGroup,
    dab_uniforms: wgpu::Buffer,
    dab_instances: wgpu::Buffer,
    dabs_this_frame: u32,
    /// The bitmap tip, or a 1x1 placeholder. Held so it outlives the bind
    /// group that references it.
    tip: wgpu::Texture,
    has_tip: bool,
    /// A coloured stamp's colour, or a 1x1 placeholder. Held so it outlives the
    /// bind group. Allocated only for the tips that carry one — see
    /// [`TIP_COLOR_FORMAT`] for what it costs when they do.
    tip_color: wgpu::Texture,
    tip_color_view: wgpu::TextureView,
    /// Which mask is in that texture, so [`CanvasRenderer::set_tip`] can tell
    /// "the same brush again" from "a different brush".
    tip_mask: Option<Arc<TipMask>>,
    /// The paper tile, or a 1x1 placeholder. Held so it outlives the bind group.
    grain: wgpu::Texture,
    grain_view: wgpu::TextureView,
    /// Which tile is in that texture, compared by `Arc` identity for exactly
    /// the reason [`CanvasRenderer::tip_mask`] is.
    grain_tile: Option<Arc<TipMask>>,
    /// The selection mask, or a 1x1 placeholder. Held so it outlives the bind
    /// group.
    selection: wgpu::Texture,
    selection_view: wgpu::TextureView,
    /// Which selection is in that texture, by `Arc` identity — the same check
    /// and the same reason as [`CanvasRenderer::tip_mask`]: comparing the
    /// coverage would cost more than the upload it saves.
    selection_mask: Option<Arc<Selection>>,
    /// The dab pass's uniforms, held rather than rebuilt: the tip and the grain
    /// are set independently, and reconstructing the block from one of them
    /// would clear the other's fields.
    dab_state: DabUniforms,
    /// Strength and tile size, so that changing only these does not rebuild a
    /// bind group or re-upload a texture.
    grain_params: (f32, f32),

    composite_bind_group: wgpu::BindGroup,
    view_uniforms: wgpu::Buffer,

    commit_bind_group: wgpu::BindGroup,
    commit_uniforms: wgpu::Buffer,

    /// The floating transform in progress, if there is one. Everything it owns
    /// is allocated when the gesture starts and given back when it ends —
    /// two canvas-sized textures and a slice of the layer array is not
    /// something to hold for a session in case somebody presses T.
    float: Option<Float>,

    /// How many times each slice's pixels have been written.
    ///
    /// **This is the layer list's invalidation rule, and it lives here because
    /// here is the only place a layer's pixels can change.** Every route — a
    /// stroke committing, a transform being put down, an undo writing a patch
    /// back, a layer cleared, a mask filled, a canvas flipped or resized — ends
    /// in one of this type's methods, so bumping a counter in each of them is
    /// exhaustive by construction. The alternative was a `touch` call beside
    /// every one of the eight call sites in `app.rs`, which is CLAUDE.md's
    /// "an invariant enforced at five call sites is one that will be forgotten
    /// at the sixth" written out in advance.
    ///
    /// Indexed by slot, and long enough for [`MAX_SLOTS`] from the start:
    /// growing it in step with the texture array would be a second place for
    /// the capacity to be got wrong. Two kilobytes — 256 `u64` is 2,048 bytes.
    /// This used to say "half a kilobyte", which was wrong by a factor of two
    /// at 129 slots as well; the figure is worth stating only because it is
    /// what makes allocating the whole thing up front obviously cheap.
    slot_revisions: Vec<u64>,
    /// The thumbnail being read back, if any. See [`ThumbJob`].
    thumb: Option<ThumbJob>,
    /// The thumbnail pass's target and staging buffer, allocated on the first
    /// request and reused for every one after it. Sixteen kilobytes each, so
    /// holding them is cheaper than the allocation churn of a per-job pair.
    thumb_target: Option<wgpu::Texture>,
    thumb_buffer: Option<wgpu::Buffer>,
    /// Which slice each of this document's effects is baked into, and the
    /// canvas-sized working set they are baked through. Empty and allocating
    /// nothing until a document has an effect.
    effects: EffectCache,
}

impl Shared {
    fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("umber-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let grain_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("umber-grain-sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        // ---- dab pass -------------------------------------------------------
        let dab_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dab"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/dab.wgsl").into()),
        });
        let dab_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dab-bgl"),
            entries: &[
                uniform_entry(0),
                texture_entry(1),
                sampler_entry(2),
                texture_entry(3),
                sampler_entry(4),
                texture_entry(5),
                texture_entry(6),
            ],
        });

        let dab_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("dab-pl"),
            bind_group_layouts: &[Some(&dab_layout)],
            immediate_size: 0,
        });
        let dab_primitive = wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..Default::default()
        };

        // One descriptor, four pipelines. `colored` decides the fragment entry
        // point and whether the colour scratch is attached; `build_up` decides
        // only the coverage target's blend state, which is the whole reason it
        // is a pipeline choice and not a shader branch.
        let dab_pipelines = std::array::from_fn(|i| {
            let style = DabStyle {
                per_dab_color: i & 1 != 0,
                build_up: i & 2 != 0,
            };
            let coverage = if style.build_up {
                COVERAGE_BUILDUP_TARGET
            } else {
                COVERAGE_TARGET
            };
            let targets: &[Option<wgpu::ColorTargetState>] = if style.per_dab_color {
                &[Some(coverage), Some(STROKE_COLOR_TARGET)]
            } else {
                &[Some(coverage)]
            };
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(match i {
                    0 => "dab-pipeline",
                    1 => "dab-colored-pipeline",
                    2 => "dab-buildup-pipeline",
                    _ => "dab-colored-buildup-pipeline",
                }),
                layout: Some(&dab_pl),
                vertex: wgpu::VertexState {
                    module: &dab_shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: DAB_VERTEX_LAYOUT,
                },
                fragment: Some(wgpu::FragmentState {
                    module: &dab_shader,
                    entry_point: Some(if style.per_dab_color {
                        "fs_colored"
                    } else {
                        "fs"
                    }),
                    compilation_options: Default::default(),
                    targets,
                }),
                primitive: dab_primitive,
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        });

        // ---- composite pass -------------------------------------------------
        //
        // `blend.wgsl` in front, so the blend modes are compiled from one text
        // shared with the commit pass rather than written out twice. See that
        // file: two copies of Multiply is exactly the drift that makes a stroke
        // jump at pointer-up.
        let composite_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("composite"),
            source: wgpu::ShaderSource::Wgsl(BLEND_PRELUDE_COMPOSITE.into()),
        });
        let composite_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("composite-bgl"),
            entries: &[
                uniform_entry(0),
                texture_array_entry(1),
                texture_entry(2),
                sampler_entry(3),
                texture_entry(4),
            ],
        });
        let composite_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("composite-pl"),
            bind_group_layouts: &[Some(&composite_layout)],
            immediate_size: 0,
        });
        // One composite *shader*, two pipelines, differing only in target
        // format. The blend maths — the thing that must never be duplicated —
        // is shared; what cannot be shared is the format a pipeline is compiled
        // against, and the window's is whatever the swapchain hands us.
        let make_composite = |label: &str, format: wgpu::TextureFormat| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&composite_pl),
                vertex: wgpu::VertexState {
                    module: &composite_shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &composite_shader,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(format.into())],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let composite_pipeline = make_composite("composite-pipeline", surface_format);
        let composite_offscreen_pipeline =
            make_composite("composite-offscreen-pipeline", OFFSCREEN_FORMAT);

        // ---- commit pass ----------------------------------------------------
        //
        // `blend.wgsl` in front of this one too — see `BLEND_PRELUDE_COMMIT`.
        // The preview and the commit call one `composite_over` rather than each
        // carrying a copy of it.
        let commit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("commit"),
            source: wgpu::ShaderSource::Wgsl(BLEND_PRELUDE_COMMIT.into()),
        });
        let commit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("commit-bgl"),
            entries: &[
                uniform_entry(0),
                texture_entry(1),
                sampler_entry(2),
                texture_entry(3),
            ],
        });

        let commit_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("commit-pl"),
            bind_group_layouts: &[Some(&commit_layout)],
            immediate_size: 0,
        });

        // Paint and erase share a shader but need different blend state.
        //
        // Paint is ordinary premultiplied source-over. Erase cannot be: with
        // `src_factor: One` the alpha channel computes
        // `a = cov + dst.a * (1 - cov)`, which *adds* opacity — an eraser that
        // paints. Zeroing the source factor gives `a = dst.a * (1 - cov)`,
        // which is what removing coverage actually means.
        let erase_blend = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Zero,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        };
        let make_commit_pipeline = |label: &str, blend: wgpu::BlendState| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&commit_pl),
                vertex: wgpu::VertexState {
                    module: &commit_shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &commit_shader,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: LAYER_FORMAT,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };

        let commit_pipeline = make_commit_pipeline(
            "commit-paint",
            wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
        );
        let commit_erase_pipeline = make_commit_pipeline(
            "commit-erase",
            wgpu::BlendState {
                color: erase_blend,
                alpha: erase_blend,
            },
        );

        // The blended commit: everything the fixed-function blender cannot do.
        //
        // Multiply needs the pixel underneath, and no combination of blend
        // factors produces `B(Cb, Cs)`, so `fs_blend` computes the whole result
        // and the target's blend is `None`. The destination it needs is bound
        // at 4 as a copy, because a colour attachment may not also be sampled
        // — the same constraint `flip.wgsl` works around.
        //
        // The uniform carries a **dynamic offset**: one block per damaged
        // piece, because each piece is drawn against its own backdrop copy and
        // the vertex shader spans the piece rather than the whole rectangle.
        // One buffer and one bind group either way; the alternative was a bind
        // group per piece, which is allocation churn on pointer-up for nothing.
        let commit_blend_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("commit-blend-bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: true,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    texture_entry(1),
                    sampler_entry(2),
                    texture_entry(3),
                    texture_entry(4),
                ],
            });
        let commit_blend_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("commit-blend-pl"),
            bind_group_layouts: &[Some(&commit_blend_layout)],
            immediate_size: 0,
        });
        let commit_blend_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("commit-blend"),
                layout: Some(&commit_blend_pl),
                vertex: wgpu::VertexState {
                    module: &commit_shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &commit_shader,
                    entry_point: Some("fs_blend"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: LAYER_FORMAT,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });

        // ---- flip pass ------------------------------------------------------
        //
        // No sampler in the layout at all: `flip.wgsl` reads with
        // `textureLoad`, and a sampler nothing uses would be a suggestion that
        // it filters. The target is `LAYER_FORMAT_LINEAR` and the blend is
        // `None`, which together are what make the pass an exact permutation.
        let flip_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("flip"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/flip.wgsl").into()),
        });
        let flip_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("flip-bgl"),
            entries: &[uniform_entry(0), texture_entry(1)],
        });
        let flip_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("flip-pl"),
            bind_group_layouts: &[Some(&flip_layout)],
            immediate_size: 0,
        });
        let flip_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("flip"),
            layout: Some(&flip_pl),
            vertex: wgpu::VertexState {
                module: &flip_shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &flip_shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: LAYER_FORMAT_LINEAR,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // ---- thumbnail pass -------------------------------------------------
        //
        // No sampler, for the reason the flip pass has none: `thumbnail.wgsl`
        // reads with `textureLoad`, because a bilinear tap at a reduction of
        // 30:1 drops nearly every texel it is meant to be summarising.
        let thumb_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("thumbnail"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/thumbnail.wgsl").into()),
        });
        let thumb_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("thumb-bgl"),
            entries: &[uniform_entry(0), texture_array_entry(1)],
        });
        let thumb_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("thumb-pl"),
            bind_group_layouts: &[Some(&thumb_layout)],
            immediate_size: 0,
        });
        let thumb_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("thumbnail"),
            layout: Some(&thumb_pl),
            vertex: wgpu::VertexState {
                module: &thumb_shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &thumb_shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    // Non-sRGB, matching every other offscreen target here: the
                    // shader does its own encode and a typed target would do it
                    // twice.
                    format: OFFSCREEN_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // ---- effect bake ----------------------------------------------------
        //
        // No sampler, for the reason the flip and thumbnail passes have none:
        // `effect.wgsl` reads with `textureLoad` throughout and does the one
        // bilinear it needs by hand. That also keeps the uint seed texture beside
        // the float ones out of any argument about filtering support, since a
        // `Uint` sample type is never filterable.
        let effect_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("effect"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/effect.wgsl").into()),
        });
        let effect_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("effect-bgl"),
            entries: &[
                // Dynamic, one block per pass: see `EffectUniforms`.
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture_array_entry(1),
                texture_entry(2),
                texture_entry(3),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Uint,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let effect_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("effect-pl"),
            bind_group_layouts: &[Some(&effect_layout)],
            immediate_size: 0,
        });
        // Seven pipelines from one loop over one descriptor, for the reason the
        // four dab pipelines are: they differ in an entry point and a target
        // format, and seven copies of a descriptor is seven places for the rest
        // of it to drift.
        let effect_pipeline = |label: &str, entry: &str, format: wgpu::TextureFormat| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&effect_pl),
                vertex: wgpu::VertexState {
                    module: &effect_shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &effect_shader,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        // Every pass writes every texel of its own viewport, so
                        // there is nothing to blend with and a blend state would
                        // only be a dependency the driver has to honour.
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let effect_extract = effect_pipeline("effect-extract", "fs_extract", STROKE_FORMAT);
        let effect_seed = effect_pipeline("effect-seed", "fs_seed", SEED_FORMAT);
        let effect_step = effect_pipeline("effect-step", "fs_step", SEED_FORMAT);
        let effect_grow = effect_pipeline("effect-grow", "fs_grow", STROKE_FORMAT);
        let effect_down = effect_pipeline("effect-down", "fs_down", STROKE_FORMAT);
        let effect_box = effect_pipeline("effect-box", "fs_box", STROKE_FORMAT);
        let effect_resolve = effect_pipeline("effect-resolve", "fs_resolve", LAYER_FORMAT);

        // ---- transform pass -------------------------------------------------
        let transform_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("transform"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/transform.wgsl").into()),
        });
        let transform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("transform-bgl"),
            entries: &[
                uniform_entry(0),
                texture_entry(1),
                sampler_entry(2),
                texture_entry(3),
            ],
        });
        let transform_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("transform-pl"),
            bind_group_layouts: &[Some(&transform_layout)],
            immediate_size: 0,
        });

        // The two mask pipelines differ only in their blend state, and the
        // blend state is the whole of what they do: `fs_mask` writes coverage
        // into alpha and zero into colour, so with the source factor zeroed the
        // target is scaled by the mask or by its complement. Written as one
        // closure rather than two descriptors, for the reason the dab
        // pipelines are.
        let make_transform_pipeline = |label: &str, entry: &str, blend: wgpu::BlendState| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&transform_pl),
                vertex: wgpu::VertexState {
                    module: &transform_shader,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &transform_shader,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: LAYER_FORMAT,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let scale_by = |dst: wgpu::BlendFactor| {
            let c = wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Zero,
                dst_factor: dst,
                operation: wgpu::BlendOperation::Add,
            };
            wgpu::BlendState { color: c, alpha: c }
        };
        let transform_keep_pipeline = make_transform_pipeline(
            "transform-keep",
            "fs_mask",
            scale_by(wgpu::BlendFactor::SrcAlpha),
        );
        let transform_take_pipeline = make_transform_pipeline(
            "transform-take",
            "fs_mask",
            scale_by(wgpu::BlendFactor::OneMinusSrcAlpha),
        );
        let transform_draw_pipeline = make_transform_pipeline(
            "transform-draw",
            "fs_sample",
            wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
        );

        Self {
            sampler,
            grain_sampler,
            dab_pipelines,
            dab_layout,
            composite_pipeline,
            composite_offscreen_pipeline,
            composite_layout,
            commit_layout,
            commit_pipeline,
            commit_erase_pipeline,
            commit_blend_layout,
            commit_blend_pipeline,
            flip_layout,
            flip_pipeline,
            transform_layout,
            thumb_layout,
            thumb_pipeline,
            effect_layout,
            effect_extract,
            effect_seed,
            effect_step,
            effect_grow,
            effect_down,
            effect_box,
            effect_resolve,
            transform_keep_pipeline,
            transform_take_pipeline,
            transform_draw_pipeline,
        }
    }
}

impl CanvasRenderer {
    pub fn new(
        device: &wgpu::Device,
        doc_size: UVec2,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        Self::with_shared(device, doc_size, Shared::new(device, surface_format))
    }

    /// A renderer for a second document, reusing this one's compiled pipelines.
    ///
    /// Layer storage is emphatically *not* shared: each document owns its own
    /// texture array and its own stroke scratch, so switching tabs is a
    /// different renderer rather than a reallocation and a re-upload. What is
    /// shared is everything that would otherwise be recompiled — see [`Shared`].
    ///
    /// The new renderer's textures hold whatever the allocation contained, so
    /// the caller must clear them before the first composite, exactly as it
    /// does after [`CanvasRenderer::new`] — and set the new document's
    /// background, which is its own and not this one's.
    pub fn for_document(&self, device: &wgpu::Device, doc_size: UVec2) -> Self {
        Self::with_shared(device, doc_size, self.shared.clone())
    }

    fn with_shared(device: &wgpu::Device, doc_size: UVec2, shared: Shared) -> Self {
        let layers = LayerStore::new(device, doc_size, initial_slots(slice_bytes(doc_size)));

        let stroke = make_stroke_texture(device, doc_size);
        let stroke_view = stroke.create_view(&wgpu::TextureViewDescriptor::default());

        // A 1x1 stand-in, exactly as the tip has. Nearly every stroke paints one
        // colour and never touches this, so a document-sized allocation here
        // would be megabytes held for a feature most sessions never use.
        let stroke_color = make_stroke_color_texture(device, UVec2::ONE);
        let stroke_color_view = stroke_color.create_view(&wgpu::TextureViewDescriptor::default());

        let dab_uniforms = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("dab-uniforms"),
            contents: bytemuck::bytes_of(&DabUniforms::plain(doc_size)),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let dab_instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dab-instances"),
            size: DAB_STRIDE * MAX_DABS_PER_FRAME as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // A 1x1 placeholder stands in when no tip is set, so the bind group
        // layout never varies and there is still exactly one dab pipeline. Its
        // contents do not matter: with `use_tip` at zero the shader samples it
        // and discards the result, which is the price of keeping
        // `textureSample` out of non-uniform control flow.
        let tip = make_tip_texture(device, 1, 1);
        let tip_view = tip.create_view(&wgpu::TextureViewDescriptor::default());
        // The same placeholder trick for the paper. Its contents do not matter
        // either: with `grain_strength` at zero the shader's `mix` returns
        // exactly 1.0 whatever was sampled.
        let grain = make_tip_texture(device, 1, 1);
        let grain_view = grain.create_view(&wgpu::TextureViewDescriptor::default());
        // A placeholder again, but for a different reason: this one is never
        // sampled, because `use_selection` is zero and the shader's `select`
        // returns 1.0 without looking. It exists so the bind group layout does
        // not vary — there is still exactly one set of dab pipelines.
        let selection = make_coverage_texture(device, 1, 1, "umber-selection-mask");
        let selection_view = selection.create_view(&wgpu::TextureViewDescriptor::default());
        // The tip's colour, when it has one. A placeholder for the tip's own
        // reason rather than the selection's: `use_tip_color` is zero, so
        // `fs_colored` samples it and throws the answer away, and `fs` — every
        // ordinary stroke — does not sample it at all.
        let tip_color = make_tip_color_texture(device, 1, 1);
        let tip_color_view = tip_color.create_view(&wgpu::TextureViewDescriptor::default());
        let dab_bind_group = make_dab_bind_group(
            device,
            &shared.dab_layout,
            &dab_uniforms,
            &tip_view,
            &shared.sampler,
            &grain_view,
            &shared.grain_sampler,
            &selection_view,
            &tip_color_view,
        );

        let view_uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("view-uniforms"),
            size: std::mem::size_of::<ViewUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let composite_bind_group = make_composite_bind_group(
            device,
            &shared.composite_layout,
            &view_uniforms,
            &layers.array_view,
            &stroke_view,
            &shared.sampler,
            &stroke_color_view,
        );

        let commit_uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("commit-uniforms"),
            size: std::mem::size_of::<CommitUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let commit_bind_group = make_commit_bind_group(
            device,
            &shared.commit_layout,
            &commit_uniforms,
            &stroke_view,
            &shared.sampler,
            &stroke_color_view,
        );

        Self {
            doc_size,
            // Transparent until the caller says otherwise, which is what the
            // canvas looked like before documents had a background at all.
            background: Background::Transparent.premultiplied(),
            shared,
            layers,
            stroke,
            stroke_view,
            stroke_color,
            stroke_color_view,
            has_stroke_color: false,
            probes: (0..PROBE_SLOTS)
                .map(|i| Probe {
                    buffer: device.create_buffer(&wgpu::BufferDescriptor {
                        label: Some(&format!("umber-probe-{i}")),
                        size: (PROBE_ROW_BYTES * PROBE_SIZE) as u64,
                        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                        mapped_at_creation: false,
                    }),
                    state: ProbeState::Idle,
                    outcome: Arc::new(AtomicU8::new(PROBE_PENDING)),
                    stale: false,
                })
                .collect(),
            capture: None,
            readback_limit: device.limits().max_buffer_size,
            dab_bind_group,
            dab_uniforms,
            dab_instances,
            dabs_this_frame: 0,
            tip,
            has_tip: false,
            tip_color,
            tip_color_view,
            tip_mask: None,
            grain,
            grain_view,
            grain_tile: None,
            selection,
            selection_view,
            selection_mask: None,
            grain_params: (0.0, 1.0),
            dab_state: DabUniforms::plain(doc_size),
            composite_bind_group,
            view_uniforms,
            commit_bind_group,
            commit_uniforms,
            float: None,
            slot_revisions: vec![0; MAX_SLOTS],
            thumb: None,
            thumb_target: None,
            thumb_buffer: None,
            // Nothing is allocated until a document actually has an effect: a
            // canvas-sized working set is 400 MB at 10000², and the great
            // majority of documents never ask for one.
            effects: EffectCache::default(),
        }
    }

    pub fn doc_size(&self) -> UVec2 {
        self.doc_size
    }

    /// Set what lies under this document's layer stack.
    ///
    /// Costs a field write: the value reaches the GPU with the rest of the view
    /// uniforms on the next composite, so changing it mid-frame is free and
    /// dragging a colour picker over it does not touch a buffer.
    pub fn set_background(&mut self, background: Background) {
        self.background = background.premultiplied();
    }

    pub fn slot_capacity(&self) -> u32 {
        self.layers.capacity
    }

    /// Guarantee at least `needed` texture-array slices exist.
    ///
    /// Growth reallocates the array and copies every existing slice, so it
    /// doubles rather than growing by one — a document that reaches eight
    /// layers pays for two copies, not eight. **Only while the array is cheap**:
    /// past a byte budget it grows by whole quanta of that budget instead,
    /// because a slice is canvas-sized and doubling one is not an optimisation
    /// but a gigabyte. At 10000² a quantum is a single slice, so growth there is
    /// exact. [`grown_capacity`] is the whole policy and has the argument.
    ///
    /// **The `.min` below fails open and the assertion is what stops it.**
    /// Asked for more than [`MAX_SLOTS`], this allocates the ceiling, logs a
    /// growth line naming it, and returns as though it had done what it was
    /// asked — after which every slot at or above the ceiling indexes off the
    /// end of the array. It is unreachable today, because `SlotPool` hands out
    /// at most slot 255 and `begin_float` refuses at the ceiling; but this
    /// clamp is the *only* thing standing between those two guarantees and
    /// silently wrong pixels, and `LayerStack::MAX_SLOTS` lives in a different
    /// crate, where it can be raised on its own with only one test in this one
    /// to notice. A named failure is worth the debug build's comparison.
    pub fn ensure_slots(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, needed: u32) {
        debug_assert!(
            needed <= MAX_SLOTS as u32,
            "asked for {needed} slices against a ceiling of {MAX_SLOTS}"
        );
        if needed <= self.layers.capacity {
            return;
        }
        let capacity = grown_capacity(
            self.layers.capacity,
            needed,
            // `self.doc_size`, which `resize` rewrites before anything can ask
            // again, so the budget is always against the canvas the array is
            // actually being built for.
            slice_bytes(self.doc_size),
        )
        .min(MAX_SLOTS as u32);
        log::info!(
            "growing layer storage {} -> {} slots",
            self.layers.capacity,
            capacity
        );

        let grown = LayerStore::new(device, self.doc_size, capacity);

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("grow-layers"),
        });
        enc.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.layers.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &grown.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: self.doc_size.x,
                height: self.doc_size.y,
                depth_or_array_layers: self.layers.capacity,
            },
        );
        // Slices beyond the old capacity are freshly allocated and hold
        // whatever the driver left behind.
        for slot in self.layers.capacity..capacity {
            clear_view(&mut enc, &grown.slot_views[slot as usize], "clear-new-slot");
        }
        queue.submit(Some(enc.finish()));

        self.composite_bind_group = make_composite_bind_group(
            device,
            &self.shared.composite_layout,
            &self.view_uniforms,
            &grown.array_view,
            &self.stroke_view,
            &self.shared.sampler,
            &self.stroke_color_view,
        );
        self.layers = grown;
    }

    /// Change the canvas size, carrying the artwork across.
    ///
    /// Every one of this document's textures is sized to the canvas, so a
    /// resize reallocates all of them and copies the surviving rectangle into
    /// the new ones. Where that rectangle lands is [`CanvasCopy::plan`]'s to
    /// decide, in `umber-core`, so the app's preview of a resize and what the
    /// GPU actually does cannot drift apart.
    ///
    /// The layer array is copied **whole**, all slices in one transfer: the
    /// anchor moves the picture, not one layer relative to another, so the
    /// origin is the same for every slice and the depth of the copy is the slot
    /// capacity. The new array is cleared first, because the region outside the
    /// surviving rectangle is freshly allocated memory.
    ///
    /// Two things the caller owes this:
    ///
    /// * **No stroke in flight.** The scratch is thrown away rather than
    ///   resampled — a half-painted stroke has no meaning at a new size, and
    ///   rescaling coverage would soften the mark it is about to commit.
    /// * **Clear the undo history.** Every `PixelPatch` is a rectangle in the
    ///   *old* geometry; replaying one after a resize would paste the right
    ///   bytes into the wrong pixels, or name a rectangle off the edge. This is
    ///   the same reason deleting a layer clears it, and structural undo is the
    ///   same real fix.
    /// * **Drop the selection**, for the same reason again: its bounds are a
    ///   rectangle of the old canvas and can now name pixels that do not exist.
    ///   This drops the *mask*, so a caller that forgot leaves a document that
    ///   is unclipped rather than one clipped to the wrong place — but it also
    ///   leaves the outline on screen describing nothing, so do not forget.
    ///
    /// # A known limitation: this carries the old canvas's slice count
    ///
    /// **The array is rebuilt at `self.layers.capacity`, and that figure was
    /// decided against the canvas being left behind.** [`grown_capacity`] keeps
    /// speculative slices under [`GROWTH_DOUBLING_BUDGET_BYTES`], but the budget
    /// is in *bytes* and a resize changes what a slice costs, so a capacity that
    /// was inside it stops being. A 512² document legitimately holding 256
    /// slices is 256 MiB; resized to 2048² it is **4.29 GB**, and to 10000²,
    /// **102.4 GB** — the same figures the growth rule exists to prevent,
    /// arrived at through a dialog instead. It predates that rule and is not
    /// caused by it.
    ///
    /// **It is not fixed here because it cannot be, from inside this method.**
    /// Shrinking means allocating fewer slices than the array holds, and this
    /// type does not know which of them hold pixels — `LayerStack` does. A
    /// resize that guessed would drop layers, which is far worse than holding
    /// memory, so it deliberately holds memory.
    ///
    /// A fix needs the live slot count threaded in from the caller. That is
    /// **`App::apply_canvas`**, the one production call site, which reaches it
    /// as `self.editor.layers.slot_capacity_needed()`; not
    /// `Editor::apply_canvas`, which shares the name, returns a `bool` and never
    /// touches a renderer. With it this could rebuild at
    /// `grown_capacity(0, live, slice_bytes(new_size))` and copy only that
    /// depth. It is a signature change through one call site and it belongs in
    /// a branch about resizing rather than one about growth.
    pub fn resize(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        new_size: UVec2,
        anchor: Anchor,
    ) {
        let new_size = new_size.max(UVec2::ONE);
        if new_size == self.doc_size {
            return;
        }
        // Every thumbnail is a picture of a canvas that is about to stop
        // existing, and the one in flight would come home describing the old
        // geometry through the new document's arithmetic.
        self.touch_all_slots();
        // And every effect slice holds a picture of that canvas. Dropped rather
        // than resampled, for the reason `docs/layer-effects.md` §9.4 gives: the
        // pixels are derived, so rebuilding them is exact by definition where a
        // resample would be a promise about filtering. `touch_all_slots` above
        // has already made every entry stale; this also gives back the working
        // set, whose textures are the old canvas's size — and the bind groups
        // with it, which name a layer array this method is about to replace.
        self.effects.forget_all();
        let plan = CanvasCopy::plan(self.doc_size, new_size, anchor);
        log::info!(
            "resizing canvas {} x {} -> {} x {}, {anchor:?}",
            self.doc_size.x,
            self.doc_size.y,
            new_size.x,
            new_size.y,
        );

        let resized = LayerStore::new(device, new_size, self.layers.capacity);
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("resize-canvas"),
        });
        for view in &resized.slot_views {
            clear_view(&mut enc, view, "clear-resized-slot");
        }
        enc.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.layers.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: plan.from.x,
                    y: plan.from.y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &resized.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: plan.to.x,
                    y: plan.to.y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: plan.size.x,
                height: plan.size.y,
                depth_or_array_layers: self.layers.capacity,
            },
        );
        queue.submit(Some(enc.finish()));
        self.layers = resized;

        // The scratch is the stroke in progress, and there is not one — see the
        // contract above. Reallocated rather than copied, and it starts clear
        // like any freshly allocated target.
        self.stroke = make_stroke_texture(device, new_size);
        self.stroke_view = self
            .stroke
            .create_view(&wgpu::TextureViewDescriptor::default());
        if self.has_stroke_color {
            self.stroke_color = make_stroke_color_texture(device, new_size);
            self.stroke_color_view = self
                .stroke_color
                .create_view(&wgpu::TextureViewDescriptor::default());
        }
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("clear-resized-scratch"),
        });
        self.clear_stroke(&mut enc);
        queue.submit(Some(enc.finish()));

        // Its base and its floating copy are canvas-sized and its rectangles
        // name pixels that no longer exist. Thrown away rather than resampled,
        // for the reason the scratch is: a half-finished gesture has no meaning
        // at a new size, and the caller owes this no stroke and no float in
        // flight anyway.
        self.end_float();
        // A sample recorded against the old canvas would be read back as if it
        // belonged to the new one.
        self.reset_probes();
        // And a capture half-read against the old canvas would be assembled
        // into a file with layers of two different sizes in it.
        self.cancel_capture();

        self.doc_size = new_size;
        self.dab_state.doc_size = [new_size.x as f32, new_size.y as f32];
        // The mask names pixels of a canvas that no longer exists. Dropped
        // rather than rescaled: a selection is the artist's statement about
        // where they are working, and a resampled one is a guess.
        // `set_selection` rebuilds the bind group, so this must run before the
        // uniform write rather than after it.
        self.set_selection(device, queue, None);
        queue.write_buffer(&self.dab_uniforms, 0, bytemuck::bytes_of(&self.dab_state));

        self.composite_bind_group = make_composite_bind_group(
            device,
            &self.shared.composite_layout,
            &self.view_uniforms,
            &self.layers.array_view,
            &self.stroke_view,
            &self.shared.sampler,
            &self.stroke_color_view,
        );
        self.commit_bind_group = make_commit_bind_group(
            device,
            &self.shared.commit_layout,
            &self.commit_uniforms,
            &self.stroke_view,
            &self.shared.sampler,
            &self.stroke_color_view,
        );
    }

    /// Set the bitmap tip the dab pass stamps, or `None` for the procedural
    /// round brush.
    ///
    /// The tip is bound for the whole dab pass rather than carried per dab, so
    /// a thousand tipped dabs are still a single draw call. Change it *between*
    /// strokes: a stroke has one brush, and swapping mid-pass would restamp the
    /// dabs already in the scratch under the new shape.
    ///
    /// What the tip does is modulate coverage. It is not composited and it does
    /// not touch the blend state, so overlapping tipped dabs still saturate at
    /// 1.0 and stroke opacity is still applied once, at commit —
    /// `a_tipped_stamp_still_saturates_under_overlap` guards that.
    ///
    /// Called at the start of every stroke, so it early-outs when the mask has
    /// not changed. The test is `Arc` **identity**, not equality: masks are
    /// shared out of the brush library, so two brushes cut from one stamp
    /// really are one allocation, and comparing a megabyte of coverage to
    /// answer "same brush as last time?" would put the cost back. Without the
    /// guard a texture allocation and a copy land on the first frame of every
    /// stroke, which is the one moment this project exists to keep short.
    ///
    /// `stamps_color` is whether a **coloured stamp**'s colour should be
    /// honoured, and it is a parameter rather than being read off the mask on
    /// purpose. It has to be the *same decision* as
    /// [`StrokeStyle::per_dab_color`], which turns on for a smudging brush as
    /// well — so a brush that smudges *and* carries a coloured tip would
    /// otherwise take the coloured pipeline for one reason and stamp its tip's
    /// colour for another, in cases where the caller had refused the second.
    /// An eraser and a stroke on a mask are exactly those cases: neither has
    /// anywhere for a colour to land, a mask is read on `.r`, and the result
    /// was a mask stroke that previewed grey and committed the stamp's red.
    /// One argument, decided once by the caller, is what stops the two.
    pub fn set_tip(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tip: Option<Arc<TipMask>>,
        stamps_color: bool,
    ) {
        // What the colour plane is about to be. Part of the early-out, because
        // the same brush can come back with the answer changed — pick up the
        // eraser without changing tip and the stamp must stop colouring.
        let want_color = stamps_color && tip.as_ref().is_some_and(|mask| mask.is_coloured());
        let same_mask = match (&self.tip_mask, &tip) {
            (Some(current), Some(next)) => Arc::ptr_eq(current, next),
            (None, None) => true,
            _ => false,
        };
        if same_mask && self.dab_state.use_tip_color == u32::from(want_color) {
            return;
        }

        let (texture, has_tip) = match &tip {
            Some(mask) => {
                // The mask's own proportions. Padding it into a square would
                // reach the same geometry and pay for an empty margin in
                // texture memory and in fragments — see `TipMask::aspect`.
                let (sx, sy) = mask.aspect();
                self.dab_state.tip_scale = [sx, sy];
                self.dab_state.use_tip = 1;
                (upload_mask(device, queue, mask), true)
            }
            None => {
                self.dab_state.tip_scale = [1.0, 1.0];
                self.dab_state.use_tip = 0;
                (make_tip_texture(device, 1, 1), false)
            }
        };

        // A coloured stamp's second plane. Uploaded here rather than lazily,
        // because it is bound for the whole dab pass exactly as the coverage is
        // — and dropped back to the placeholder the moment a tip without one is
        // chosen, or the moment the caller declines it, so a session that
        // touched one coloured brush does not go on holding its pixels.
        let color = want_color
            .then_some(tip.as_ref())
            .flatten()
            .and_then(|mask| mask.colour_premultiplied().map(|rgba| (mask, rgba)));
        let color_texture = match color {
            Some((mask, rgba)) => upload_tip_color(device, queue, mask, &rgba),
            None => make_tip_color_texture(device, 1, 1),
        };
        self.dab_state.use_tip_color = u32::from(want_color);
        self.tip_color_view = color_texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.tip_color = color_texture;

        self.tip = texture;
        self.has_tip = has_tip;
        self.tip_mask = tip;
        self.rebuild_dab_bind_group(device);
        queue.write_buffer(&self.dab_uniforms, 0, bytemuck::bytes_of(&self.dab_state));
    }

    /// Whether the dab pass will stamp the tip's own colour.
    ///
    /// This is [`Self::set_tip`]'s `stamps_color` **and** the tip actually
    /// carrying a colour, which is the one thing that has to agree with
    /// [`StrokeStyle::per_dab_color`]: a stroke that stamped a colour without
    /// the colour attachment attached would have it thrown away and commit as
    /// the flat palette colour. The caller decides both from one snapshot — see
    /// `Editor::begin_stroke` — so this reports rather than requires.
    pub fn stamps_tip_color(&self) -> bool {
        self.dab_state.use_tip_color != 0
    }

    /// Set the paper the dab pass bites through, or `None` for none.
    ///
    /// Per stroke, exactly as [`Self::set_tip`] is and for the same reasons: one
    /// binding covers a whole dab pass, and changing it mid-stroke would leave
    /// the dabs already in the scratch textured by the previous paper.
    ///
    /// The tile is compared by `Arc` identity, so calling this every stroke with
    /// the same paper costs a pointer comparison. `strength` and `scale` are
    /// compared by value and cost a uniform write when they change — no texture
    /// upload and no bind group, which is what makes dragging the Texture
    /// section's sliders cheap.
    ///
    /// A strength of zero is the **exact identity**: the shader computes
    /// `mix(1.0, tile, strength)`, which at zero is 1.0 whatever the tile holds.
    /// `grain_off_is_the_exact_identity` is the guard, and it is why an ordinary
    /// brush pays one multiply rather than a branch.
    pub fn set_grain(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        grain: Option<(Arc<TipMask>, f32, f32)>,
    ) {
        let (tile, strength, scale) = match grain {
            Some((tile, strength, scale)) => (Some(tile), strength.clamp(0.0, 1.0), scale.max(1.0)),
            // Nothing to bind and nothing to sample: leave whatever tile is
            // already uploaded where it is and turn the strength off. A painter
            // who reaches for grain once will reach for it again, and dropping
            // the texture would mean re-uploading it on the next stroke.
            None => (self.grain_tile.clone(), 0.0, self.dab_state.grain_scale),
        };

        let same_tile = match (&self.grain_tile, &tile) {
            (Some(current), Some(next)) => Arc::ptr_eq(current, next),
            (None, None) => true,
            _ => false,
        };
        if same_tile && self.grain_params == (strength, scale) {
            return;
        }

        if !same_tile {
            let texture = match &tile {
                Some(mask) => upload_mask(device, queue, mask),
                None => make_tip_texture(device, 1, 1),
            };
            self.grain_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.grain = texture;
            self.grain_tile = tile;
            self.rebuild_dab_bind_group(device);
        }

        self.grain_params = (strength, scale);
        self.dab_state.grain_strength = strength;
        self.dab_state.grain_scale = scale;
        queue.write_buffer(&self.dab_uniforms, 0, bytemuck::bytes_of(&self.dab_state));
    }

    /// Clip the dab pass to `selection`, or to nothing at all with `None`.
    ///
    /// Per stroke, exactly as [`Self::set_tip`] and [`Self::set_grain`] are, and
    /// for the same reason: one binding covers a whole dab pass, so changing it
    /// mid-stroke would leave the coverage already in the scratch clipped by the
    /// selection that has gone. The mask is compared by `Arc` identity, so
    /// calling this at the start of every stroke costs a pointer comparison.
    ///
    /// **The selection is applied here and nowhere else.** The scratch then
    /// holds coverage that is already clipped, so the preview and the commit —
    /// which must implement identical blending maths — cannot disagree about
    /// where the selection was, because neither of them knows there is one.
    pub fn set_selection(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        selection: Option<Arc<Selection>>,
    ) {
        let unchanged = match (&self.selection_mask, &selection) {
            (Some(current), Some(next)) => Arc::ptr_eq(current, next),
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return;
        }

        let texture = match &selection {
            Some(sel) => {
                let rect = sel.bounds();
                self.dab_state.sel_min = [rect.x as f32, rect.y as f32];
                self.dab_state.sel_size = [rect.width as f32, rect.height as f32];
                self.dab_state.use_selection = 1;
                upload_coverage(
                    device,
                    queue,
                    rect.width,
                    rect.height,
                    sel.coverage(),
                    "umber-selection-mask",
                )
            }
            None => {
                self.dab_state.sel_min = [0.0, 0.0];
                // Not zero: the shader divides by this, and a NaN would take
                // the whole dab with it rather than merely being discarded.
                self.dab_state.sel_size = [1.0, 1.0];
                self.dab_state.use_selection = 0;
                make_coverage_texture(device, 1, 1, "umber-selection-mask")
            }
        };

        self.selection_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.selection = texture;
        self.selection_mask = selection;
        self.rebuild_dab_bind_group(device);
        queue.write_buffer(&self.dab_uniforms, 0, bytemuck::bytes_of(&self.dab_state));
    }

    fn rebuild_dab_bind_group(&mut self, device: &wgpu::Device) {
        let tip_view = self
            .tip
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.dab_bind_group = make_dab_bind_group(
            device,
            &self.shared.dab_layout,
            &self.dab_uniforms,
            &tip_view,
            &self.shared.sampler,
            &self.grain_view,
            &self.shared.grain_sampler,
            &self.selection_view,
            &self.tip_color_view,
        );
    }

    /// Whether a bitmap tip is currently bound.
    pub fn has_tip(&self) -> bool {
        self.has_tip
    }

    /// Reset the per-frame instance cursor. Call once at the top of a frame.
    pub fn begin_frame(&mut self) {
        self.dabs_this_frame = 0;
    }

    /// Give the stroke somewhere to record a colour per dab.
    ///
    /// Allocated the first time a smudging stroke needs it and kept thereafter:
    /// a painter who reaches for a blender once will reach for it again, and
    /// re-allocating a document-sized texture per stroke would be a stutter at
    /// exactly the wrong moment. The two bind groups that name it have to be
    /// rebuilt, which is why this is not simply a lazy getter.
    fn ensure_stroke_color(&mut self, device: &wgpu::Device) {
        if self.has_stroke_color {
            return;
        }
        self.stroke_color = make_stroke_color_texture(device, self.doc_size);
        self.stroke_color_view = self
            .stroke_color
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.has_stroke_color = true;

        self.composite_bind_group = make_composite_bind_group(
            device,
            &self.shared.composite_layout,
            &self.view_uniforms,
            &self.layers.array_view,
            &self.stroke_view,
            &self.shared.sampler,
            &self.stroke_color_view,
        );
        self.commit_bind_group = make_commit_bind_group(
            device,
            &self.shared.commit_layout,
            &self.commit_uniforms,
            &self.stroke_view,
            &self.shared.sampler,
            &self.stroke_color_view,
        );
    }

    /// Upload dabs and stamp them into the scratch texture.
    ///
    /// `style` must be the **same for every frame of a stroke**, and its
    /// `per_dab_color` must match the [`StrokeStyle`] handed to
    /// [`Self::composite`] and [`Self::commit_stroke`]. Turning colour on midway
    /// would leave the earlier dabs with no colour recorded, and they would
    /// commit as the flat palette colour while the rest smudged; turning
    /// build-up on midway would leave the first half of the stroke saturating
    /// where the second half accumulates, which is a visible step in a mark that
    /// should be even.
    pub fn draw_dabs(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        dabs: &[Dab],
        style: DabStyle,
    ) {
        if dabs.is_empty() {
            return;
        }
        let colored = style.per_dab_color;
        if colored {
            self.ensure_stroke_color(device);
        }
        let room = MAX_DABS_PER_FRAME.saturating_sub(self.dabs_this_frame as usize);
        if room == 0 {
            log::warn!("dab instance buffer full, dropping {} dabs", dabs.len());
            return;
        }
        let dabs = &dabs[..dabs.len().min(room)];

        let offset = self.dabs_this_frame as u64 * DAB_STRIDE;
        queue.write_buffer(&self.dab_instances, offset, bytemuck::cast_slice(dabs));

        // Load, never clear: the scratch accumulates across frames for the whole
        // stroke, so only the new dabs are drawn each frame.
        let load = wgpu::Operations {
            load: wgpu::LoadOp::Load,
            store: wgpu::StoreOp::Store,
        };
        let coverage_attachment = Some(wgpu::RenderPassColorAttachment {
            view: &self.stroke_view,
            resolve_target: None,
            depth_slice: None,
            ops: load,
        });
        let color_attachment = Some(wgpu::RenderPassColorAttachment {
            view: &self.stroke_color_view,
            resolve_target: None,
            depth_slice: None,
            ops: load,
        });
        let attachments: &[Option<wgpu::RenderPassColorAttachment<'_>>] = if colored {
            &[coverage_attachment, color_attachment]
        } else {
            std::slice::from_ref(&coverage_attachment)
        };

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("dab-pass"),
            color_attachments: attachments,
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.shared.dab_pipelines[style.index()]);
        pass.set_bind_group(0, &self.dab_bind_group, &[]);
        pass.set_vertex_buffer(
            0,
            self.dab_instances
                .slice(offset..offset + dabs.len() as u64 * DAB_STRIDE),
        );
        pass.draw(0..4, 0..dabs.len() as u32);
        drop(pass);

        self.dabs_this_frame += dabs.len() as u32;
    }

    /// Draw the whole layer stack plus the in-progress stroke to `target`.
    pub fn composite(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        params: &CompositeParams<'_>,
    ) {
        let scale = 1.0 / params.camera.zoom;
        // Solving `doc = screen * scale + offset` for the pivot mapping to the
        // camera centre. Must stay in step with `Camera::screen_to_doc`, which
        // the input path uses — if they disagree, strokes land off the cursor.
        let offset = params.camera.center - params.pivot * scale;

        // Against [`MAX_DRAWS`], not [`MAX_LAYERS`]: `params.layers` is the
        // *draw* list the app flattened folders out of, which a layer's effects
        // will each add an entry to.
        let mut packed = [[0.0f32; 4]; MAX_DRAWS];
        let mut extra = [[0.0f32; 4]; MAX_DRAWS];
        let count = params.layers.len().min(MAX_DRAWS);
        for ((dst, ext), src) in packed
            .iter_mut()
            .zip(extra.iter_mut())
            .zip(&params.layers[..count])
        {
            *dst = [
                src.opacity.clamp(0.0, 1.0),
                src.blend as f32,
                src.slot as f32,
                if src.visible { 1.0 } else { 0.0 },
            ];
            *ext = [
                // The layer's own slice where there is no mask, so the array
                // index the shader samples is always in range and the result
                // is discarded by the flag beside it rather than by a branch.
                src.mask.unwrap_or(src.slot) as f32,
                if src.mask.is_some() { 1.0 } else { 0.0 },
                if src.clipped { 1.0 } else { 0.0 },
                0.0,
            ];
        }

        let color = params.stroke.color;
        queue.write_buffer(
            &self.view_uniforms,
            0,
            bytemuck::bytes_of(&ViewUniforms {
                scale: [scale, scale],
                offset: [offset.x, offset.y],
                doc_size: [self.doc_size.x as f32, self.doc_size.y as f32],
                pivot: [params.pivot.x, params.pivot.y],
                stroke_color: [
                    color.r,
                    color.g,
                    color.b,
                    params.stroke.opacity.clamp(0.0, 1.0),
                ],
                backdrop: [
                    params.backdrop[0],
                    params.backdrop[1],
                    params.backdrop[2],
                    1.0,
                ],
                background: self.background,
                layer_count: count as u32,
                stroke_mode: mode_index(params.stroke.mode),
                active_index: params.active_index,
                checker: 8.0,
                is_export: if params.export { 1 } else { 0 },
                per_dab_color: u32::from(params.stroke.per_dab_color),
                stroke_on_mask: u32::from(params.stroke.on_mask),
                // The brush's mode, and it reaches the shader for the paint
                // path alone — an eraser's branch never reads it. Not clamped
                // or coerced here: `Brush::blend_applies` is the one place that
                // decision is made, and restating it would be a second place
                // for the preview and the commit to disagree.
                stroke_blend: params.stroke.blend.index(),
                layers: packed,
                extra,
            }),
        );

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("composite-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        // `export` means both "straight alpha, no checkerboard" and "this is
        // not the window", and the two always travel together: every offscreen
        // target Umber composites into is `OFFSCREEN_FORMAT`.
        pass.set_pipeline(if params.export {
            &self.shared.composite_offscreen_pipeline
        } else {
            &self.shared.composite_pipeline
        });
        pass.set_bind_group(0, &self.composite_bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Bake the scratch stroke into `slot` over `rect`, then clear the scratch.
    /// Bake the finished stroke into the layer.
    ///
    /// `pieces` are the parts of `rect` the stroke actually reached, and the
    /// pass is scissored to them: **exactly the pixels the undo patch was
    /// captured from, and no others.** That equality is the whole of why a
    /// patch may be smaller than the stroke's bounding box. Committing the
    /// whole box instead would run every pixel of it through the blend — an
    /// identity where coverage is zero, but an identity computed in floating
    /// point and written back through an sRGB encode, which is a guarantee
    /// about rounding rather than a guarantee about pixels. Scissoring makes it
    /// a guarantee about pixels: an untouched cell is never written at all.
    ///
    /// It is also less work. A thin diagonal across a large canvas commits a
    /// hundred and fifty narrow strips instead of the whole document.
    ///
    /// A brush carrying a blend mode other than [`BlendMode::Normal`] goes down
    /// [`Self::commit_blended`] instead. Normal — every stroke there has ever
    /// been — is untouched: the same one pass, the same fixed-function blender,
    /// no copy and no allocation.
    ///
    /// The device is here because the blended path allocates — a backdrop
    /// texture, a uniform block per piece and a bind group, all dropped when it
    /// returns. Bundling the arguments to please the lint would hide which of
    /// them the two paths share; `rect` and `pieces` are separately meaningful
    /// (the first is what the quad spans, the second what survives the scissor)
    /// and the style is already the one struct preview and commit share.
    #[allow(clippy::too_many_arguments)]
    pub fn commit_stroke(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        slot: u32,
        rect: PixelRect,
        pieces: &[PixelRect],
        style: StrokeStyle,
    ) {
        let Some(view) = self.layers.slot_views.get(slot as usize) else {
            log::error!("commit to slot {slot} beyond capacity");
            return;
        };

        // Two strokes carry no blend, and both are *ignored* rather than
        // refused — the same reading `umber_core::Brush::blend_applies` gives,
        // and the editor never sends one here in either case.
        //
        // An eraser has no colour, so it has nothing to blend with what is
        // under it. A stroke on a mask has no colour either: the slice holds
        // coverage on one channel, and `fs_blend` writes four, so a blended
        // commit onto one would put colour into a mask. Guarding only the
        // eraser is the asymmetry that gets forgotten — a caller reaching
        // `commit_stroke` directly is all that stands between the two.
        let blends = style.mode == BrushMode::Paint && !style.on_mask;
        if blends && style.blend != BlendMode::Normal {
            self.commit_blended(device, encoder, slot, pieces, style);
            self.clear_stroke(encoder);
            self.touch_slot(slot);
            return;
        }

        let color = style.color;
        queue.write_buffer(
            &self.commit_uniforms,
            0,
            bytemuck::bytes_of(&CommitUniforms {
                rect_min: [rect.x as f32, rect.y as f32],
                rect_max: [(rect.x + rect.width) as f32, (rect.y + rect.height) as f32],
                doc_size: [self.doc_size.x as f32, self.doc_size.y as f32],
                _pad0: [0.0; 2],
                color: [color.r, color.g, color.b, style.opacity.clamp(0.0, 1.0)],
                mode: mode_index(style.mode),
                per_dab_color: u32::from(style.per_dab_color),
                blend: BlendMode::Normal.index(),
                _pad2: 0,
            }),
        );

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("commit-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(match style.mode {
                BrushMode::Paint => &self.shared.commit_pipeline,
                BrushMode::Erase => &self.shared.commit_erase_pipeline,
            });
            pass.set_bind_group(0, &self.commit_bind_group, &[]);
            // One quad, drawn once per piece under a different scissor. The
            // vertex shader spans `rect` every time — the scissor is what
            // decides which of it survives — so nothing per piece has to reach
            // the uniform buffer.
            for piece in pieces {
                pass.set_scissor_rect(piece.x, piece.y, piece.width, piece.height);
                pass.draw(0..4, 0..1);
            }
        }

        self.clear_stroke(encoder);
        self.touch_slot(slot);
    }

    /// The commit for a brush whose blend mode is not Normal.
    ///
    /// # Why this needs a copy at all
    ///
    /// Multiply is a function of the pixel underneath, and no combination of
    /// fixed-function blend factors can produce one — `B(Cb, Cs)` is not linear
    /// in the destination for Overlay, and even Multiply's premultiplied form
    /// needs the source twice. So the destination has to arrive as a *sampled*
    /// input, and a colour attachment may not also be bound for sampling. The
    /// pass therefore reads a copy, which is the arrangement `flip.wgsl` uses
    /// for the same reason.
    ///
    /// # Why the copy is per piece
    ///
    /// A backdrop covering the whole damaged rectangle would be canvas-sized
    /// for a stroke drawn across the picture, and — much worse — it would be
    /// canvas-sized for a *thin diagonal* too, since that stroke's bounding box
    /// is the whole document. That is the 381 MB the tiled undo patch exists to
    /// avoid, put back on the GPU. A `TileMask` piece is a contiguous *run* of
    /// cells within one row of the 64-pixel damage grid — `push_run` emits one
    /// per run, so a row may hold several — and each is therefore never taller
    /// than a cell nor wider than the stroke's own rectangle. That is what
    /// bounds the copy at `canvas width × 64` however long the stroke is; the
    /// texture is sized to the largest single piece.
    ///
    /// A caller that hands over the bounding rectangle as one piece gets a
    /// backdrop the size of it, and that is the honest bound rather than a
    /// hole in the one above: the undo patch for that same commit is the whole
    /// rectangle too, so the backdrop is never larger than what the caller was
    /// already paying for on the CPU.
    ///
    /// The cost is a render pass per piece rather than one pass with a scissor
    /// per piece, because a copy cannot be recorded inside a pass. Since a piece
    /// is a run rather than a row, that count follows how much the stroke
    /// zig-zags and not only how long it is — a hundred and fifty passes for a
    /// thin diagonal across the largest canvas, and several times that for a
    /// stroke that crosses its own row repeatedly. Once, at pointer-up, on a
    /// path that already does a blocking readback for the undo patch, and only
    /// for a brush that asked for a blend mode.
    ///
    /// That argument forbids *interleaving* copies and passes; it does not
    /// forbid recording every copy first and then drawing one pass. Copying the
    /// pieces into a single atlas and drawing them under the scissor and dynamic
    /// offset that already exist would be one pass, and is the change to make if
    /// this ever needs to be cheaper — batched to a byte budget, because the
    /// total piece area is 6.8 MB for that diagonal and 381 MB for a wash that
    /// covers the canvas. It is not worth it on the desktop, where the scissor
    /// makes an extra pass nearly free. It would matter on a tile-based
    /// renderer, where wgpu's render area is the whole attachment and each pass
    /// loads and stores every tile of the slice — which is Android and iOS, and
    /// neither has ever been built.
    ///
    /// Everything allocated here is dropped when the commit returns. A stroke
    /// is not often enough to be worth caching, and caching would mean holding
    /// a canvas-wide texture for a session because one stroke wanted it.
    fn commit_blended(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        slot: u32,
        pieces: &[PixelRect],
        style: StrokeStyle,
    ) {
        let Some(view) = self.layers.slot_views.get(slot as usize) else {
            return;
        };
        // A zero-sized piece would be an illegal copy extent and draws nothing
        // anyway. `pieces` never holds one today; skipping is cheaper than
        // depending on that.
        let live: Vec<PixelRect> = pieces
            .iter()
            .copied()
            .filter(|p| p.width > 0 && p.height > 0)
            .collect();
        if live.is_empty() {
            return;
        }

        let widest = live.iter().map(|p| p.width).max().unwrap_or(1);
        let tallest = live.iter().map(|p| p.height).max().unwrap_or(1);
        let backdrop = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("umber-commit-backdrop"),
            size: wgpu::Extent3d {
                width: widest,
                height: tallest,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: LAYER_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let backdrop_view = backdrop.create_view(&wgpu::TextureViewDescriptor::default());

        // One uniform block per piece, because the vertex shader spans the
        // piece rather than the whole rectangle: the backdrop copy sits at the
        // texture's origin, so `rect_min` is what maps a fragment into it.
        // Rounded *up to* the alignment rather than `max`ed with it: a dynamic
        // offset must itself be a multiple of the alignment, and a `max` only
        // happens to give one while the block is smaller than 256 bytes. Grow
        // `CommitUniforms` past that and every piece after the first would take
        // an unaligned offset — a validation error on a canvas with two damaged
        // pieces and on no other, which is not the first thing anybody tests.
        let stride = std::mem::size_of::<CommitUniforms>()
            .next_multiple_of(device.limits().min_uniform_buffer_offset_alignment as usize);
        let color = style.color;
        let mut blocks = vec![0u8; stride * live.len()];
        for (i, piece) in live.iter().enumerate() {
            let block = CommitUniforms {
                rect_min: [piece.x as f32, piece.y as f32],
                rect_max: [
                    (piece.x + piece.width) as f32,
                    (piece.y + piece.height) as f32,
                ],
                doc_size: [self.doc_size.x as f32, self.doc_size.y as f32],
                _pad0: [0.0; 2],
                color: [color.r, color.g, color.b, style.opacity.clamp(0.0, 1.0)],
                mode: mode_index(style.mode),
                per_dab_color: u32::from(style.per_dab_color),
                blend: style.blend.index(),
                _pad2: 0,
            };
            let at = i * stride;
            blocks[at..at + std::mem::size_of::<CommitUniforms>()]
                .copy_from_slice(bytemuck::bytes_of(&block));
        }
        let uniforms = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("umber-commit-blend-uniforms"),
            contents: &blocks,
            usage: wgpu::BufferUsages::UNIFORM,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("commit-blend-bg"),
            layout: &self.shared.commit_blend_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &uniforms,
                        offset: 0,
                        // One block, not the whole buffer: with a dynamic
                        // offset the bound range is `offset .. offset + size`,
                        // and binding the lot would run off the end.
                        size: wgpu::BufferSize::new(std::mem::size_of::<CommitUniforms>() as u64),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&self.stroke_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.shared.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&self.stroke_color_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&backdrop_view),
                },
            ],
        });

        for (i, piece) in live.iter().enumerate() {
            // The copy has to precede the pass that reads it, and a copy cannot
            // be recorded inside one. Pieces never overlap, so piece *i* reads
            // pixels no earlier piece wrote.
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.layers.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: piece.x,
                        y: piece.y,
                        z: slot,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &backdrop,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: piece.width,
                    height: piece.height,
                    depth_or_array_layers: 1,
                },
            );

            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("commit-blend-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.shared.commit_blend_pipeline);
            pass.set_bind_group(0, &bind_group, &[(i * stride) as u32]);
            // The quad already covers exactly this piece, so the scissor is
            // belt and braces — and it is what keeps "no pixel outside the
            // pieces the undo patch was captured from is written" a property of
            // the pass rather than of the rasteriser's rounding.
            pass.set_scissor_rect(piece.x, piece.y, piece.width, piece.height);
            pass.draw(0..4, 0..1);
        }
    }

    /// Wipe the scratch surface.
    ///
    /// Both halves of it. Leaving stale colour behind would be the same class
    /// of bug as leaving stale coverage: the next smudging stroke would pick up
    /// the previous one's smear wherever its own dabs had not yet reached.
    pub fn clear_stroke(&self, encoder: &mut wgpu::CommandEncoder) {
        clear_view(encoder, &self.stroke_view, "clear-stroke");
        if self.has_stroke_color {
            clear_view(encoder, &self.stroke_color_view, "clear-stroke-colour");
        }
    }

    /// Wipe one layer.
    pub fn clear_layer(&mut self, encoder: &mut wgpu::CommandEncoder, slot: u32) {
        if let Some(view) = self.layers.slot_views.get(slot as usize) {
            clear_view(encoder, view, "clear-layer");
        }
        self.touch_slot(slot);
    }

    /// Fill one slice with opaque white — what a **new mask** starts as.
    ///
    /// White is "reveal everything", so a layer that has just gained a mask
    /// looks exactly as it did a moment before; that is what makes adding one
    /// something a painter can try rather than commit to.
    ///
    /// A clear rather than a draw. The clear value is linear and the target is
    /// sRGB-typed, but 1.0 encodes to 255 either way, so the slice really does
    /// come back as `0xff` in every channel — which matters, because the
    /// composite reads the red one and a mask that arrived at 0xfe would dim
    /// its layer by a level the painter never asked for.
    pub fn fill_layer_white(&mut self, encoder: &mut wgpu::CommandEncoder, slot: u32) {
        self.touch_slot(slot);
        let Some(view) = self.layers.slot_views.get(slot as usize) else {
            return;
        };
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("fill-mask-white"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }

    /// Wipe every allocated slot. Used at startup.
    pub fn clear_all_layers(&mut self, encoder: &mut wgpu::CommandEncoder) {
        for view in &self.layers.slot_views {
            clear_view(encoder, view, "clear-layer");
        }
        self.touch_all_slots();
    }

    // --- flipping the canvas ------------------------------------------------

    /// Mirror `slots` about the canvas's centre line, in place.
    ///
    /// **Exactly reversible, texel for texel.** That is the requirement, not a
    /// nicety: the history entry a flip records stores no pixels at all and is
    /// undone by flipping again ([`umber_core::EditBody::Flip`]), so anything
    /// lossy here would move the picture a little every time somebody flipped
    /// and undid. `flip.wgsl` has the three things that make it exact — integer
    /// `textureLoad`, non-sRGB views on both sides, and no blending.
    ///
    /// A texture cannot be its own render attachment and
    /// `copy_texture_to_texture` cannot mirror, so each slice is drawn into one
    /// scratch texture and copied straight back. The scratch is canvas-sized
    /// and lives only for the call: a flip is an explicit command, not
    /// something the drawing path does, and holding a spare canvas for the rest
    /// of the session in case somebody presses the key would cost every
    /// document that never does.
    ///
    /// The canvas size does not change, which is the whole reason a flip can
    /// keep the undo history where a resize cannot. Nothing here reallocates.
    ///
    /// The caller owes this **no stroke and no float in flight** — the scratch
    /// surface and the floating copy are not mirrored, so a stroke would commit
    /// unmirrored over the flipped picture and a preview would put its pixels
    /// down in the place they were dragged to before the flip.
    pub fn flip_layers(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        slots: &[u32],
        axis: FlipAxis,
    ) {
        if slots.is_empty() {
            return;
        }
        // A sample recorded against the picture as it was would be read back as
        // though it belonged to the picture as it is.
        self.reset_probes();
        // And a capture part-way through would assemble a file out of layers
        // that were mirrored and layers that were not. The scheduler's half of
        // this is the caller's — see `app.rs`'s `stop_autosave_of`.
        self.cancel_capture();
        for &slot in slots {
            self.touch_slot(slot);
        }

        let scratch = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("umber-flip"),
            size: wgpu::Extent3d {
                width: self.doc_size.x,
                height: self.doc_size.y,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: LAYER_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[LAYER_FORMAT_LINEAR],
        });
        let scratch_view = scratch.create_view(&wgpu::TextureViewDescriptor {
            label: Some("umber-flip-raw"),
            format: Some(LAYER_FORMAT_LINEAR),
            ..Default::default()
        });

        let uniforms = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("flip-uniforms"),
            contents: bytemuck::bytes_of(&FlipUniforms {
                doc_size: [self.doc_size.x, self.doc_size.y],
                axis: match axis {
                    FlipAxis::Horizontal => 0,
                    FlipAxis::Vertical => 1,
                },
                _pad: 0,
            }),
            usage: wgpu::BufferUsages::UNIFORM,
        });

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("flip-canvas"),
        });
        for &slot in slots {
            let Some(source) = self.layers.raw_slot_views.get(slot as usize) else {
                log::error!("flip of slot {slot} beyond capacity");
                continue;
            };
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("flip-bg"),
                layout: &self.shared.flip_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniforms.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(source),
                    },
                ],
            });
            {
                // `Clear` rather than `Load`: every texel of the scratch is
                // written by the pass, so loading whatever the last slice left
                // there would only be a dependency the driver has to honour.
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("flip-slice"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &scratch_view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&self.shared.flip_pipeline);
                pass.set_bind_group(0, &bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
            // A raw byte copy, so the trip back through the array costs the
            // picture nothing. Commands within one encoder run in order, so
            // this reads what the pass above wrote and the next slice's pass
            // may reuse the scratch.
            enc.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &scratch,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &self.layers.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: slot,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: self.doc_size.x,
                    height: self.doc_size.y,
                    depth_or_array_layers: 1,
                },
            );
        }
        queue.submit(Some(enc.finish()));
    }

    // --- floating transforms ------------------------------------------------

    /// The layer whose slice the composite pass must be shown the preview slice
    /// for instead, and that slice — or `None` when nothing is floating.
    ///
    /// This is the whole of how a floating transform reaches the screen. The
    /// caller swaps the slot in its `LayerDraw` for this one and
    /// `composite.wgsl` is untouched: the preview slice holds exactly what the
    /// layer will hold once the float is put down, so it composites at the
    /// right position, under the right blend mode, at the right opacity,
    /// without any of that being restated here. See [`Float`].
    pub fn float_preview(&self) -> Option<(u32, u32)> {
        self.float.as_ref().map(|f| (f.layer_slot, f.preview_slot))
    }

    pub fn float_in_flight(&self) -> bool {
        self.float.is_some()
    }

    /// Pick pixels up off a layer, or put pasted ones down over it, ready to be
    /// dragged about.
    ///
    /// `reserved` is the document's slot high-water mark — everything the layer
    /// stack might use — because the preview needs a slice of the same array
    /// and must not take one a layer could later be given. Returns the preview
    /// slice, or `None` when there is no room for it: a document already using
    /// every slice the shader's array has cannot also hold a preview, and
    /// refusing is better than previewing into a layer.
    ///
    /// Submits twice, deliberately. A paste arrives through
    /// `Queue::write_texture`, whose writes are flushed *before* the command
    /// buffers of the submission they precede — so clearing the floating copy
    /// in the same encoder would wipe the pixels that were just written into
    /// it. The clear therefore goes in its own submission. This runs once per
    /// gesture, where `start_stroke` already submits.
    pub fn begin_float(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        reserved: u32,
        source: &FloatSource<'_>,
    ) -> Option<u32> {
        self.end_float();
        // Against [`MAX_SLOTS`], not [`MAX_LAYERS`]: `reserved` counts *slices*
        // — a layer, a layer's mask — and the array holds twice the stack's
        // entries, plus one, plus the effect-draw headroom. That `+ 1` is this
        // preview's spare. Comparing against 64 refused every document past its
        // 64th slice: 33 masked layers could not be transformed at all, with 63
        // slices free, under a notice that said Umber had run out.
        //
        // **This is reachable**, and used not to be. `reserved` is one past the
        // highest slice *claimed*, and structural undo parks a deleted layer's
        // slice in the entry that could put it back — so a history competes for
        // the range. The caller gives entries up before asking; `App::
        // free_headroom` is that release, and it declines to spend the history
        // where the live stack itself reaches the ceiling, because no eviction
        // can help there. That state needs a live layer holding a slot *number*
        // at the top of the range, which parking puts it there; it is not "64
        // layers each with a mask", which is 128 slices and has never reached
        // any ceiling this constant has had.
        if reserved as usize >= MAX_SLOTS {
            log::error!("no room for a transform preview beside {reserved} layer slices");
            return None;
        }
        let preview_slot = reserved;
        self.ensure_slots(device, queue, preview_slot + 1);
        if source.slot >= self.layers.capacity {
            log::error!("transform of slot {} beyond capacity", source.slot);
            return None;
        }

        let base = self.make_float_texture(device, "umber-float-base");
        let base_view = base.create_view(&wgpu::TextureViewDescriptor::default());
        let floating = self.make_float_texture(device, "umber-float-source");
        let floating_view = floating.create_view(&wgpu::TextureViewDescriptor::default());

        // Snapshotted rather than shared with the dab pass's mask: the two
        // features never run at once, and a binding reached across would tie a
        // live transform to whatever the next stroke set.
        let (mask, mask_min, mask_size, use_mask) = match source.mask {
            Some(sel) => {
                let r = sel.bounds();
                (
                    upload_coverage(
                        device,
                        queue,
                        r.width,
                        r.height,
                        sel.coverage(),
                        "umber-float-mask",
                    ),
                    [r.x as f32, r.y as f32],
                    [r.width as f32, r.height as f32],
                    1,
                )
            }
            // Not zero for the size: the shader divides by it, and a NaN would
            // take the whole quad with it rather than merely being discarded.
            None => (
                make_coverage_texture(device, 1, 1, "umber-float-mask"),
                [0.0, 0.0],
                [1.0, 1.0],
                0,
            ),
        };
        let mask_view = mask.create_view(&wgpu::TextureViewDescriptor::default());

        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("transform-uniforms"),
            size: std::mem::size_of::<TransformUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let make_bind_group = |source: &wgpu::TextureView, label: &str| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &self.shared.transform_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniforms.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.shared.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&mask_view),
                    },
                ],
            })
        };
        let bind_group = make_bind_group(&floating_view, "transform-bg");
        // The mask passes need the layer as it stands, because what they compute
        // is a *share* of what is already there rather than the mask on its own
        // — see `fs_mask`, and the ghost outline that reading the mask alone
        // left behind. It has to be the layer's own slice and not either of
        // their targets: the base and the floating copy are colour attachments
        // here, an exclusive usage, and wgpu refuses a pass that also samples
        // one. The layer is untouched until the commit, so it is the one
        // pristine copy both passes can share.
        let mask_bind_group = make_bind_group(
            &self.layers.slot_views[source.slot as usize],
            "transform-mask-bg",
        );

        // First submission: the floating copy starts empty, whatever the
        // allocation held. See the note on this function.
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("begin-float-clear"),
        });
        clear_view(&mut enc, &floating_view, "clear-float-source");
        queue.submit(Some(enc.finish()));

        let lifting = source.pixels.is_none();
        if let Some(pixels) = source.pixels {
            write_rect(
                queue,
                &floating,
                wgpu::Origin3d {
                    x: source.rect.x,
                    y: source.rect.y,
                    z: 0,
                },
                source.rect,
                pixels,
            );
        }

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("begin-float"),
        });
        // The base is the layer as the float will sit on it. Copied whole
        // because the drag can carry the picture anywhere on the canvas, and
        // every later frame restores out of this.
        enc.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.layers.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: source.slot,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &base,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: self.doc_size.x,
                height: self.doc_size.y,
                depth_or_array_layers: 1,
            },
        );
        if lifting {
            enc.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.layers.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: source.rect.x,
                        y: source.rect.y,
                        z: source.slot,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &floating,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: source.rect.x,
                        y: source.rect.y,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width: source.rect.width,
                    height: source.rect.height,
                    depth_or_array_layers: 1,
                },
            );
        }

        // One write, two passes: the mask is applied to the floating copy and
        // its complement to the base, over the same rectangle with the same
        // mask, so both read the same uniforms. Two writes here would be a bug
        // — `write_buffer` is staged, so both passes in one encoder would see
        // whichever was written last.
        queue.write_buffer(
            &uniforms,
            0,
            bytemuck::bytes_of(&TransformUniforms {
                rect_min: [source.rect.x as f32, source.rect.y as f32],
                rect_max: [
                    (source.rect.x + source.rect.width) as f32,
                    (source.rect.y + source.rect.height) as f32,
                ],
                doc_size: [self.doc_size.x as f32, self.doc_size.y as f32],
                inv_x: [1.0, 0.0],
                inv_y: [0.0, 1.0],
                inv_t: [0.0, 0.0],
                mask_min,
                mask_size,
                use_mask,
                _pad0: 0.0,
                _pad1: 0.0,
                _pad2: 0.0,
            }),
        );
        if lifting {
            // A lift outside a selection takes the rectangle whole, and with
            // `use_mask` clear the shader's share is exactly 1.0 — so this pass
            // would be the identity and is skipped rather than run.
            if use_mask != 0 {
                self.mask_pass(
                    &mut enc,
                    &self.shared.transform_keep_pipeline,
                    &mask_bind_group,
                    &floating_view,
                    "float-keep",
                );
            }
            self.mask_pass(
                &mut enc,
                &self.shared.transform_take_pipeline,
                &mask_bind_group,
                &base_view,
                "float-take",
            );
        }
        // The preview starts as the base: the hole is visible the moment the
        // pixels are picked up, before anything has been dragged.
        enc.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &base,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &self.layers.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: preview_slot,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: self.doc_size.x,
                height: self.doc_size.y,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(enc.finish()));

        self.float = Some(Float {
            layer_slot: source.slot,
            preview_slot,
            base,
            source: floating,
            mask,
            uniforms,
            bind_group,
            last_dest: None,
        });
        Some(preview_slot)
    }

    /// Redraw the preview for a transform that has moved.
    ///
    /// Cheap enough for the drawing path: it restores only the rectangle the
    /// previous preview and this one between them cover, and draws only where
    /// the pixels land. Nothing is allocated.
    ///
    /// One uniform write per encoder — see [`Self::begin_float`] — so this and
    /// [`Self::commit_float`] must not share one.
    pub fn draw_float(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        params: &FloatParams,
    ) {
        let Some(float) = self.float.as_ref() else {
            return;
        };
        let (preview_slot, last) = (float.preview_slot, float.last_dest);
        if let Some(restore) = span(last, params.dest) {
            self.render_float(queue, encoder, preview_slot, restore, params);
            self.touch_slot(preview_slot);
        }
        if let Some(float) = self.float.as_mut() {
            float.last_dest = params.dest;
        }
    }

    /// Put the floating pixels down into the layer they belong to.
    ///
    /// `damage` must cover the source *and* the destination — see
    /// `Transform::damage` — and the caller must have captured that rectangle
    /// for undo before calling, exactly as `finish_stroke` does.
    ///
    /// This is [`Self::draw_float`]'s own body with the layer's slice as the
    /// target instead of the preview's, which is what makes the committed
    /// result the preview rather than a second rendering of it.
    pub fn commit_float(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        damage: PixelRect,
        params: &FloatParams,
    ) {
        let Some(slot) = self.float.as_ref().map(|f| f.layer_slot) else {
            return;
        };
        self.render_float(queue, encoder, slot, damage, params);
        self.touch_slot(slot);
    }

    /// Give the floating transform's storage back. Nothing is written: the
    /// layer was never touched, so abandoning a transform is exactly this.
    pub fn end_float(&mut self) {
        self.float = None;
    }

    /// Restore `restore` from the base and draw the floating pixels over it,
    /// into the layer-array slice `slot`.
    fn render_float(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        slot: u32,
        restore: PixelRect,
        params: &FloatParams,
    ) {
        let Some(float) = self.float.as_ref() else {
            return;
        };
        let Some(view) = self.layers.slot_views.get(slot as usize) else {
            log::error!("transform into slot {slot} beyond capacity");
            return;
        };
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &float.base,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: restore.x,
                    y: restore.y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &self.layers.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: restore.x,
                    y: restore.y,
                    z: slot,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: restore.width,
                height: restore.height,
                depth_or_array_layers: 1,
            },
        );

        let Some(dest) = params.dest else {
            return;
        };
        let columns = params.inverse.columns();
        queue.write_buffer(
            &float.uniforms,
            0,
            bytemuck::bytes_of(&TransformUniforms {
                rect_min: [dest.x as f32, dest.y as f32],
                rect_max: [(dest.x + dest.width) as f32, (dest.y + dest.height) as f32],
                doc_size: [self.doc_size.x as f32, self.doc_size.y as f32],
                inv_x: columns[0],
                inv_y: columns[1],
                inv_t: columns[2],
                // Unused by `fs_sample`: the mask was applied once, when the
                // pixels were lifted. Applying it again per frame would clip
                // the *moved* picture by where it used to be.
                mask_min: [0.0, 0.0],
                mask_size: [1.0, 1.0],
                use_mask: 0,
                _pad0: 0.0,
                _pad1: 0.0,
                _pad2: 0.0,
            }),
        );

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("float-draw"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.shared.transform_draw_pipeline);
        pass.set_bind_group(0, &float.bind_group, &[]);
        pass.draw(0..4, 0..1);
    }

    /// One of the two mask passes: scale a target by the selection's coverage
    /// or by its complement, over whatever rectangle the uniforms name.
    fn mask_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::RenderPipeline,
        bind_group: &wgpu::BindGroup,
        view: &wgpu::TextureView,
        label: &str,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..4, 0..1);
    }

    /// A canvas-sized texture in layer form, for the two a float holds.
    fn make_float_texture(&self, device: &wgpu::Device, label: &str) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: self.doc_size.x,
                height: self.doc_size.y,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: LAYER_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }

    /// A document-sized offscreen target for the export composite.
    ///
    /// Non-sRGB, matching the real surface: the shader does its own gamma
    /// encode, and an sRGB target would encode twice.
    fn export_target(&self, device: &wgpu::Device) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("umber-export"),
            size: wgpu::Extent3d {
                width: self.doc_size.x,
                height: self.doc_size.y,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OFFSCREEN_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    /// Draw the flattened document 1:1 into `view`.
    ///
    /// Factored out of [`Self::export_rgba`] because the autosave's capture
    /// needs the identical picture and must not block for it. Two spellings of
    /// "the export composite" is exactly how a saved preview starts disagreeing
    /// with an exported PNG.
    fn render_export(
        &self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        layers: &[LayerDraw],
    ) {
        // Zoom 1 with the pivot at the document centre makes screen and
        // document coordinates identical, so the render is 1:1.
        let camera = Camera {
            center: Vec2::new(self.doc_size.x as f32 * 0.5, self.doc_size.y as f32 * 0.5),
            zoom: 1.0,
        };
        self.composite(
            queue,
            encoder,
            view,
            &CompositeParams {
                camera: &camera,
                pivot: camera.center,
                layers,
                // No stroke in flight: exporting mid-stroke should write what
                // is committed, not a half-finished dab.
                active_index: 0,
                stroke: StrokeStyle {
                    opacity: 0.0,
                    ..Default::default()
                },
                backdrop: [0.0, 0.0, 0.0],
                export: true,
            },
        );
    }

    /// Flatten the visible stack to straight-alpha sRGB bytes, document-sized.
    ///
    /// Runs the same composite pass the screen uses, with its export flag set,
    /// so what lands in the file is what the canvas showed. A separate export
    /// path would be a second copy of the blend maths to keep in step.
    ///
    /// The document background is part of that, so a white-backed document
    /// exports opaque and a transparent one keeps its alpha, without this
    /// function knowing which it is.
    pub fn export_rgba(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layers: &[LayerDraw],
    ) -> Vec<u8> {
        let (w, h) = (self.doc_size.x, self.doc_size.y);

        let target = self.export_target(device);
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("export"),
        });
        self.render_export(queue, &mut encoder, &view, layers);
        // Submitted before the readback rather than sharing its encoder: the
        // readback may take several submits, and the flatten has to happen once
        // and before all of them.
        queue.submit(Some(encoder.finish()));

        self.read_texture_rows(
            device,
            queue,
            "export",
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            (w, h),
        )
    }

    /// Sample the flattened stack at one document pixel.
    ///
    /// Renders a 1×1 target rather than the whole document: an eyedropper only
    /// needs one pixel, and flattening 2048² to read four bytes would stall for
    /// milliseconds on every click. Uses the same composite pass as the screen,
    /// so the sampled colour is the one the user is looking at rather than the
    /// contents of whichever layer happens to be selected.
    ///
    /// Returns straight-alpha sRGB. A fully transparent pixel yields alpha 0,
    /// which the caller should treat as "nothing there" rather than as black.
    ///
    /// [`Self::pick_patch`] at a size of one, so the eyedropper's colour and
    /// the loupe's picture cannot come out of two different renders. See there
    /// for why the middle texel of a patch is exactly this pixel.
    pub fn pick_colour(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layers: &[LayerDraw],
        doc_point: Vec2,
    ) -> [u8; 4] {
        let px = self.pick_patch(device, queue, layers, doc_point, 1);
        [px[0], px[1], px[2], px[3]]
    }

    /// Sample a `size`×`size` block of the flattened stack, centred on one
    /// document pixel.
    ///
    /// Straight-alpha sRGB, row-major, `size * size * 4` bytes, top-left first
    /// — the eyedropper's loupe, whose neighbourhood over the canvas comes from
    /// the *document* rather than from the screen. That is the better
    /// instrument for the pixels Umber owns: the composite before the interface
    /// is drawn over it, one texel per document pixel whatever the camera is
    /// doing, so a loupe over a canvas at 37% shows the pixels a release would
    /// take rather than what the sampler resolved them into.
    ///
    /// **The camera is snapped to the middle of the pixel `doc_point` is in,
    /// and without that this whole function is a lie.** `composite.wgsl` samples
    /// the layer array through a `Linear` filter at `uv = doc / doc_size`, so a
    /// texel centre lands on a *document* pixel only where the coordinate is
    /// `n + 0.5`. `screen_to_doc` is a camera transform and hands over an
    /// arbitrary fraction, so an unsnapped block is 121 bilinear blends of four
    /// pixels each — a hard edge in the picture arrives in the loupe as a soft
    /// ramp, which is the one thing a magnifier is for reading. It is not only
    /// the loupe's problem: `pick_colour` has always been this render, so the
    /// colour an eyedropper *took* was a blend of four pixels at every
    /// coordinate but a pixel centre, and at `frac == 0` it was the flat
    /// average of all four. **The existing guards could not see it** because
    /// both pick at `(32.5, 32.5)`, the one fraction where a bilinear tap is
    /// the identity. `a_pick_takes_the_pixel_it_is_over_rather_than_a_blend_of_
    /// four` is the guard, and it aims deliberately at a pixel's corner.
    ///
    /// **The middle texel is exactly what [`Self::pick_colour`] answers**, and
    /// that is what the pivot is for: with an odd `size`, the fragment at
    /// `(size / 2) + 0.5` maps to the snapped centre and its neighbours to the
    /// document pixels either side. At a `size` of one it is the identity, so
    /// `pick_colour` is this function and there is one render rather than two
    /// that have to agree about which pixel is the sample.
    ///
    /// Blocking, like `pick_colour` and for the same reason — see
    /// `App::pick_this_frame`, which is the one caller allowed to do it per
    /// frame and says why.
    pub fn pick_patch(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        layers: &[LayerDraw],
        doc_point: Vec2,
        size: u32,
    ) -> Vec<u8> {
        let size = size.max(1);
        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("umber-pick"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OFFSCREEN_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());

        // The middle fragment sits at screen `(size / 2) + 0.5`; with zoom 1
        // and the pivot there, that maps exactly to the camera's centre and
        // every other texel to a whole document pixel from it. For one texel
        // this is the (0.5, 0.5) the single-pixel pick always used.
        //
        // `floor + 0.5` is the snap the docs above argue for: the middle of the
        // pixel `doc_point` falls in, which is where the `Linear` sampler's tap
        // is exact. Written here rather than at the call site because both
        // callers want it and a caller that forgot would get a plausible,
        // slightly soft answer.
        let camera = Camera {
            center: doc_point.floor() + Vec2::splat(0.5),
            zoom: 1.0,
        };
        let pivot = Vec2::splat((size / 2) as f32 + 0.5);

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("pick"),
        });
        self.composite(
            queue,
            &mut encoder,
            &view,
            &CompositeParams {
                camera: &camera,
                pivot,
                layers,
                active_index: 0,
                stroke: StrokeStyle {
                    opacity: 0.0,
                    ..Default::default()
                },
                backdrop: [0.0, 0.0, 0.0],
                export: true,
            },
        );
        // Submitted before the readback rather than sharing its encoder, for
        // `export_rgba`'s reason: the read may take more than one submission
        // and the composite has to happen once and before all of them.
        queue.submit(Some(encoder.finish()));

        self.read_texture_rows(
            device,
            queue,
            "pick",
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            (size, size),
        )
    }

    /// Ask what the canvas looks like under the brush, without waiting for it.
    ///
    /// This is [`Self::pick_colour`] with the blocking removed, and it exists
    /// for one caller: a smudging brush, which needs the canvas colour on every
    /// frame of a stroke. `pick_colour` blocks on the GPU, and a blocking read
    /// per frame during a stroke is exactly the thing this project is built to
    /// avoid — `read_layer_rect` carries the same warning for the same reason.
    ///
    /// So the answer arrives a frame or two later, through [`Self::take_probe`].
    /// The stroke it feeds is a trailing average by definition, and MyPaint's
    /// own smudge lags far more than the readback does, so the delay costs
    /// nothing visible.
    ///
    /// The composite pass is reused with the stroke *included*, so a blender
    /// scrubbed back and forth picks up its own wet paint rather than only what
    /// was on the layer when the stroke started.
    pub fn probe_canvas(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        params: &ProbeParams<'_>,
    ) {
        let ProbeParams {
            layers,
            active_index,
            stroke,
            doc_point,
            radius,
        } = *params;
        // A slot disowned by the previous stroke can come back into service as
        // soon as its map has settled, which is usually by now.
        self.reclaim_stale();
        // By index rather than by reference: `composite` below needs `&self`,
        // and a live `&mut` into `self.probes` would still be outstanding.
        let Some(index) = self.probes.iter().position(|p| p.state == ProbeState::Idle) else {
            // Every slot is still in flight. Dropping this sample is right: the
            // ones outstanding are more recent than anything a queue would hold,
            // and a smudge that lags further is worse than one that samples less
            // often.
            return;
        };
        self.probes[index].state = ProbeState::Rendering;

        let target = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("umber-probe"),
            size: wgpu::Extent3d {
                width: PROBE_SIZE,
                height: PROBE_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: PROBE_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());

        // Zoom so the brush's footprint fills the little target exactly, which
        // makes the readback an area average over what the dab covers rather
        // than a point sample that a single stray pixel could dominate.
        let camera = Camera {
            center: doc_point,
            zoom: PROBE_SIZE as f32 / (radius * 2.0).max(0.5),
        };
        self.composite(
            queue,
            encoder,
            &view,
            &CompositeParams {
                camera: &camera,
                pivot: Vec2::splat(PROBE_SIZE as f32 * 0.5),
                layers,
                active_index,
                stroke,
                backdrop: [0.0, 0.0, 0.0],
                // The export path returns straight alpha and skips the sRGB
                // encode, which is what makes the result usable as linear
                // colour with a meaningful alpha. Exactly what `pick_colour`
                // relies on, for the same reason.
                export: true,
            },
        );

        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.probes[index].buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(PROBE_ROW_BYTES),
                    rows_per_image: Some(PROBE_SIZE),
                },
            },
            wgpu::Extent3d {
                width: PROBE_SIZE,
                height: PROBE_SIZE,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Start the map for any probe whose copy has been submitted.
    ///
    /// Split from [`Self::probe_canvas`] because `map_async` may only be called
    /// on a buffer whose writes are already submitted, and the encoder holding
    /// that copy is still open when the probe is recorded.
    pub fn submit_probes(&mut self) {
        for slot in &mut self.probes {
            if slot.state != ProbeState::Rendering {
                continue;
            }
            slot.state = ProbeState::Mapping;
            slot.outcome.store(PROBE_PENDING, Ordering::Release);
            let outcome = slot.outcome.clone();
            slot.buffer
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    // A failed map is recorded rather than ignored: the slot
                    // still has to go back into service, but unmapping a buffer
                    // that was never mapped is itself an error. A smudge that
                    // misses a sample is a cosmetic loss, not a reason to take
                    // the app down.
                    let code = if result.is_ok() {
                        PROBE_MAPPED
                    } else {
                        PROBE_FAILED
                    };
                    outcome.store(code, Ordering::Release);
                });
        }
    }

    /// Collect whichever probe has come home, averaged to one linear RGBA.
    ///
    /// Polls without blocking — `PollType::Poll` returns immediately whether or
    /// not the GPU has caught up, which is the entire point.
    pub fn take_probe(&mut self, device: &wgpu::Device) -> Option<[f32; 4]> {
        let _ = device.poll(wgpu::PollType::Poll);

        let mut out = None;
        for slot in &mut self.probes {
            if slot.state != ProbeState::Mapping {
                continue;
            }
            match slot.outcome.load(Ordering::Acquire) {
                PROBE_MAPPED => {
                    // A sample belonging to a stroke that has already ended is
                    // read back and thrown away: the buffer still has to be
                    // unmapped before the slot can be used again.
                    if !slot.stale {
                        let mapped = slot.buffer.slice(..).get_mapped_range();
                        out = Some(average_probe(&mapped));
                    }
                    slot.buffer.unmap();
                }
                PROBE_FAILED => {}
                // Still in flight. Leaving it alone is the whole point — see
                // `reset_probes`.
                _ => continue,
            }
            slot.outcome.store(PROBE_PENDING, Ordering::Release);
            slot.stale = false;
            slot.state = ProbeState::Idle;
        }
        out
    }

    /// Disown every probe in flight, so no sample of the stroke that is ending
    /// can reach the next one.
    ///
    /// Note what this does *not* do: return a slot whose `map_async` is still
    /// outstanding to service. Doing that was a real crash. `probe_canvas`
    /// would hand the next stroke that slot, record a copy into it, and
    /// `queue.submit` refuses any submission touching a buffer that is mapped
    /// or awaiting a map — which is a validation error, and a validation error
    /// aborts the process. It is also the ordinary case rather than a rare one:
    /// a map only completes on a poll, so a stroke that ends between frames
    /// almost always leaves one behind.
    ///
    /// So the slot stays where it is and is merely marked stale;
    /// [`Self::take_probe`] unmaps it and returns it to service once the GPU is
    /// done with it.
    pub fn reset_probes(&mut self) {
        for slot in &mut self.probes {
            // `Rendering` means a copy is recorded but `map_async` has not been
            // called yet; the next `submit_probes` maps it and `take_probe`
            // then discards it, for the same reason.
            if slot.state != ProbeState::Idle {
                slot.stale = true;
            }
        }
        self.reclaim_stale();
    }

    /// Free any disowned slot whose map has already settled.
    ///
    /// Without this a stale slot would wait on [`Self::take_probe`], which
    /// `app.rs` only calls while a *smudging* stroke is live. Ending a smudge
    /// and then picking an ordinary brush would leave both slots parked for the
    /// rest of the session, and the next blender would never sample anything.
    fn reclaim_stale(&mut self) {
        for slot in &mut self.probes {
            if slot.state != ProbeState::Mapping || !slot.stale {
                continue;
            }
            match slot.outcome.load(Ordering::Acquire) {
                PROBE_MAPPED => slot.buffer.unmap(),
                PROBE_FAILED => {}
                _ => continue,
            }
            slot.outcome.store(PROBE_PENDING, Ordering::Release);
            slot.stale = false;
            slot.state = ProbeState::Idle;
        }
    }

    // --- layer thumbnails ---------------------------------------------------

    /// How many times slot `slot` has been written to.
    ///
    /// The layer list's whole invalidation rule: a thumbnail is stale exactly
    /// when this has moved since it was taken. See
    /// [`CanvasRenderer::slot_revisions`] for why the counter lives here.
    pub fn slot_revision(&self, slot: u32) -> u64 {
        self.slot_revisions.get(slot as usize).copied().unwrap_or(0)
    }

    /// Note that a slice's pixels have changed.
    ///
    /// Called by every method here that writes one, and by nothing outside this
    /// type. A thumbnail of that slice in flight is disowned in the same
    /// breath: it is a picture of the layer as it was a moment ago, and drawing
    /// it would show the stroke that has just landed as missing.
    fn touch_slot(&mut self, slot: u32) {
        if let Some(rev) = self.slot_revisions.get_mut(slot as usize) {
            *rev += 1;
        }
        if let Some(job) = self.thumb.as_mut()
            && job.slot == slot
        {
            job.abandoned = true;
        }
    }

    /// Note that every slice has changed — a flip, a resize, a fresh document.
    fn touch_all_slots(&mut self) {
        for rev in &mut self.slot_revisions {
            *rev += 1;
        }
        self.cancel_thumb();
    }

    /// True while a thumbnail is in flight, abandoned or otherwise.
    ///
    /// Abandoned counts, for the reason [`Self::capture_in_flight`] says: the
    /// staging buffer is the GPU's until its map settles.
    pub fn thumb_in_flight(&self) -> bool {
        self.thumb.is_some()
    }

    /// True when the thumbnail in flight has finished its bounds pass and is
    /// waiting to draw its picture.
    ///
    /// Exists for one test — the one that pins what happens when a layer is
    /// written *between* the two passes, which is the gap a stroke's commit
    /// lands in every time. Nothing in the application asks.
    #[doc(hidden)]
    pub fn thumb_phase_is_picture(&self) -> bool {
        self.thumb
            .as_ref()
            .is_some_and(|job| job.phase == ThumbPhase::Picture)
    }

    /// Start reading a thumbnail of `slot` back, without blocking.
    ///
    /// Returns false when one is already in flight — the caller's cue to ask
    /// again next frame rather than to queue a second. Nothing is recorded
    /// here: [`Self::drive_thumb`] records a pass, [`Self::submit_thumb`] maps
    /// it, and [`Self::take_thumb`] collects it and lets the next pass go.
    pub fn begin_thumb(&mut self, slot: u32) -> bool {
        if self.thumb.is_some() || slot >= self.layers.capacity {
            return false;
        }
        self.thumb = Some(ThumbJob {
            slot,
            revision: self.slot_revision(slot),
            phase: ThumbPhase::Bounds,
            region: None,
            state: StepState::Waiting,
            outcome: Arc::new(AtomicU8::new(PROBE_PENDING)),
            abandoned: false,
        });
        true
    }

    /// Record the pass this thumbnail is waiting on, into the frame's encoder.
    ///
    /// Costs one draw over 64² fragments and one 16 KB copy. The draw reads
    /// every texel of the region exactly once between them, which is the same
    /// bandwidth the composite pass spends on that layer every frame anyway.
    pub fn drive_thumb(&mut self, device: &wgpu::Device, encoder: &mut wgpu::CommandEncoder) {
        let Some(job) = self.thumb.as_ref() else {
            return;
        };
        if job.state != StepState::Waiting || job.abandoned {
            return;
        }
        let (slot, phase, region) = (job.slot, job.phase, job.region);
        // The whole slice for the bounds pass; what that found for the picture.
        let region = match (phase, region) {
            (ThumbPhase::Bounds, _) => umber_core::Rect::new(
                Vec2::ZERO,
                Vec2::new(self.doc_size.x as f32, self.doc_size.y as f32),
            ),
            (ThumbPhase::Picture, Some(region)) => region,
            // A picture phase with no region cannot arise — `take_thumb` sets
            // one or finishes the job — but a silent wrong picture is worse
            // than a dropped one.
            (ThumbPhase::Picture, None) => return,
        };

        let target = self.thumb_target.get_or_insert_with(|| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("umber-thumbnail"),
                size: wgpu::Extent3d {
                    width: THUMB_SIZE,
                    height: THUMB_SIZE,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: OFFSCREEN_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let buffer = self.thumb_buffer.get_or_insert_with(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("umber-thumbnail-readback"),
                size: (THUMB_ROW_BYTES * THUMB_SIZE) as u64,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });

        let uniforms = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("thumb-uniforms"),
            contents: bytemuck::bytes_of(&ThumbUniforms {
                src_min: [region.min.x, region.min.y],
                src_size: [region.max.x - region.min.x, region.max.y - region.min.y],
                dest: [THUMB_SIZE, THUMB_SIZE],
                layer_size: [self.doc_size.x, self.doc_size.y],
                slot,
                reduce: u32::from(phase == ThumbPhase::Bounds),
                _pad0: 0,
                _pad1: 0,
            }),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("thumb-bg"),
            layout: &self.shared.thumb_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&self.layers.array_view),
                },
            ],
        });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("thumbnail-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        // Every texel is written by the draw, so loading the
                        // last job's picture would only be a dependency the
                        // driver has to honour.
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.shared.thumb_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(THUMB_ROW_BYTES),
                    rows_per_image: Some(THUMB_SIZE),
                },
            },
            wgpu::Extent3d {
                width: THUMB_SIZE,
                height: THUMB_SIZE,
                depth_or_array_layers: 1,
            },
        );

        if let Some(job) = self.thumb.as_mut() {
            job.state = StepState::Rendering;
        }
    }

    /// Start the map for a thumbnail pass whose copy has been submitted.
    ///
    /// Split from [`Self::drive_thumb`] for the reason [`Self::submit_probes`]
    /// is split from `probe_canvas`: `map_async` may only be called on a buffer
    /// whose writes are already submitted, and the encoder holding that copy is
    /// still open when the pass is recorded.
    pub fn submit_thumb(&mut self) {
        let Some(job) = self.thumb.as_mut() else {
            return;
        };
        if job.state != StepState::Rendering {
            return;
        }
        let Some(buffer) = self.thumb_buffer.as_ref() else {
            return;
        };
        job.state = StepState::Mapping;
        job.outcome.store(PROBE_PENDING, Ordering::Release);
        let outcome = job.outcome.clone();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let code = if result.is_ok() {
                    PROBE_MAPPED
                } else {
                    PROBE_FAILED
                };
                outcome.store(code, Ordering::Release);
            });
    }

    /// Collect a thumbnail that has come home, if one has.
    ///
    /// Polls without blocking. A `Some` is the picture; the bounds pass in
    /// between produces nothing and merely arms the second pass, so a caller
    /// asks every frame and gets an answer every few.
    pub fn take_thumb(&mut self, device: &wgpu::Device) -> Option<Thumbnail> {
        let _ = device.poll(wgpu::PollType::Poll);

        // A disowned job is dropped as soon as nothing is outstanding on the
        // GPU, **whatever state it is in** — which is the whole of why this is
        // here rather than folded into the `Mapping` arm below.
        //
        // The bounds pass leaves the job `Waiting` at the end of one frame and
        // the picture pass is not recorded until the next, so every route that
        // writes a layer — a stroke committing, an undo, a clear, a flip —
        // lands in that gap routinely and marks it. Left in `Waiting` it would
        // be refused by `drive_thumb` (abandoned), by `submit_thumb` (not
        // `Rendering`) and by the test below (not `Mapping`), so `self.thumb`
        // would stay `Some` for the life of the renderer: no thumbnail would
        // ever update again, and `thumb_in_flight` would request a redraw every
        // frame for ever — the exact "the app never gets to wait" regression
        // `render`'s `repaint_at` exists to prevent. `take_capture` has always
        // checked its own flag at the top for the same reason.
        let job = self.thumb.as_mut()?;
        if job.abandoned && job.state != StepState::Mapping {
            // `Rendering` means a copy is recorded but no map is outstanding,
            // so the buffer is free the moment nothing intends to map it —
            // and dropping the job is what stops `submit_thumb` doing so.
            self.thumb = None;
            return None;
        }
        if job.state != StepState::Mapping {
            return None;
        }
        let buffer = self.thumb_buffer.as_ref()?;
        let mut bytes = None;
        match job.outcome.load(Ordering::Acquire) {
            PROBE_MAPPED => {
                // Read even when abandoned: the buffer still has to be unmapped
                // before the next job can be given it, and reading is what
                // makes the unmap legal to reason about.
                if !job.abandoned {
                    bytes = Some(buffer.slice(..).get_mapped_range().to_vec());
                }
                buffer.unmap();
            }
            PROBE_FAILED => {}
            // Still in flight. Leaving it alone is the whole point.
            _ => return None,
        }
        job.outcome.store(PROBE_PENDING, Ordering::Release);
        job.state = StepState::Waiting;

        let (slot, revision, phase, abandoned) = (job.slot, job.revision, job.phase, job.abandoned);
        let Some(bytes) = bytes else {
            // Abandoned or failed: the buffer is back, so the job goes.
            self.thumb = None;
            return None;
        };
        if abandoned {
            self.thumb = None;
            return None;
        }

        match phase {
            ThumbPhase::Bounds => {
                let content = umber_core::thumbnail::content_rect(
                    &bytes,
                    THUMB_ROW_BYTES as usize,
                    UVec2::splat(THUMB_SIZE),
                    self.doc_size,
                );
                match content {
                    Some(content) => {
                        let job = self.thumb.as_mut()?;
                        job.region = Some(umber_core::thumbnail::framed(
                            content,
                            UVec2::splat(THUMB_SIZE),
                        ));
                        job.phase = ThumbPhase::Picture;
                        None
                    }
                    // Nothing on the layer. Answered rather than left to time
                    // out, so the list can draw its "empty" state and stop
                    // asking: a job that produced no answer would be requested
                    // again on the very next frame, for ever.
                    None => {
                        self.thumb = None;
                        Some(Thumbnail {
                            slot,
                            revision,
                            rgba: Vec::new(),
                        })
                    }
                }
            }
            ThumbPhase::Picture => {
                self.thumb = None;
                Some(Thumbnail {
                    slot,
                    revision,
                    rgba: bytes,
                })
            }
        }
    }

    /// Disown the thumbnail in flight, if there is one.
    ///
    /// Marked rather than dropped, for the reason [`Self::reset_probes`] gives
    /// at length: a buffer awaiting a map is still the GPU's, and recording a
    /// copy into one is a validation error and therefore an abort.
    pub fn cancel_thumb(&mut self) {
        if let Some(job) = self.thumb.as_mut() {
            job.abandoned = true;
        }
    }

    // --- layer effects ------------------------------------------------------

    /// How many effects the last [`Self::bake_effects`] could not draw.
    ///
    /// Non-zero means the document holds more enabled effects than there are
    /// slices to read them out of, and **the panel is meant to say so.** A
    /// control that lights up and does nothing is what this project refuses
    /// everywhere; the honest shape is that the effect stays enabled and the
    /// document is said to be over its budget. See `docs/layer-effects.md` §6.1a
    /// for why this cannot be a refusal instead: `restore_shape` puts a deleted
    /// layer back with the effects it had, and an undo that declines to undo is
    /// worse than a picture missing a shadow.
    pub fn effects_dropped(&self) -> usize {
        self.effects.dropped
    }

    /// How many effect bakes this renderer has run. Observation only.
    ///
    /// The only way a test can say "that frame rebaked nothing", which is the
    /// whole of the cache's contract and is otherwise invisible: a stale bake and
    /// a fresh one of the same parameters produce the same pixels.
    pub fn effect_bakes(&self) -> u64 {
        self.effects.bakes
    }

    /// Whether *any* bake may run every frame on a canvas this size.
    ///
    /// See [`EFFECT_LIVE_PIXELS`]. **Nothing outside this type calls it yet**, and
    /// it is `pub` because a panel explaining why a shadow lags on a large canvas
    /// would ask exactly this — said plainly rather than dressed as a caller that
    /// exists, which is the claim `Transform::reseat`'s form refuses.
    /// [`Self::effect_bakes_live`] is the per-effect question and is the one the
    /// bake actually asks.
    pub fn effects_bake_live(&self) -> bool {
        u64::from(self.doc_size.x) * u64::from(self.doc_size.y) <= EFFECT_LIVE_PIXELS
    }

    /// Whether **this** effect may be rebaked every frame of a stroke.
    ///
    /// Above [`EFFECT_LIVE_PIXELS`] the answer is per effect rather than one gate
    /// on the canvas, and `docs/layer-effects.md` §5.1 is why: what cannot hold at
    /// canvas scale is "memory at canvas scale and the stroke's distance field,
    /// not the shadow and not the frame budget". The measurements bear that out —
    /// at 10000² a 16 px outline is 20.4 ms and over a 60 Hz frame while a soft
    /// shadow is 4.1 ms and comfortably inside one.
    ///
    /// A canvas-wide gate was what shipped first and it is worse in a way an
    /// artist cannot see: one expensive outline anywhere in the stack would switch
    /// the live rebake off for a cheap shadow on another layer, so the shadow
    /// following the brush would depend on a setting somewhere else.
    ///
    /// **The criterion is the distance field and nothing else**, which is §5.1's
    /// claim taken literally: what cannot hold at canvas scale is the flood's
    /// 20–34 ms *and* its 800 MB of seed textures, "not the shadow and not the
    /// frame budget". It is also the common case, since a drop shadow's default
    /// spread is zero.
    ///
    /// A second clause on the blur was tried and is **refused as over-fitting**.
    /// It read "downsampled, or absent", which sounds like it names the cheap
    /// blurs and does the opposite: the default 5 px shadow blurs at full
    /// resolution with a box radius of 2, and that is the *cheapest* bake there is
    /// — 7.6 ms at 10000² — so the clause refused it while admitting a 64 px one.
    /// Fixing it means a threshold on the radius, and the honest boundary is
    /// somewhere between box radius 4 (8.2 ms) and 16 (22.5 ms), which is a cliff
    /// in the middle of the range nobody could predict from the control.
    ///
    /// So the known worst is stated instead: a shadow around 31 px of softness
    /// blurs full-resolution at radius 15 and is **22.5 ms** at 10000², over a
    /// 60 Hz frame on its own. That is a stroke at 45 frames a second rather than
    /// a broken one, and stage 3's region-bounded rebake is what removes it.
    ///
    /// **There is no bound on the *count*.** Above the threshold this admits any
    /// number of canvas-scale bakes in one frame where the canvas-wide gate
    /// admitted none, so 127 hard-offset shadows at 10000² is 254 full-canvas
    /// passes a frame against the 4.1 ms the table quotes for one. Not a bound
    /// worth inventing here: stage 3's region-bounded rebake is what makes the
    /// question go away, and a cap would be a second budget with no way to say
    /// which effects it dropped.
    fn effect_bakes_live(&self, effect: &Effect) -> bool {
        effect_bakes_live_at(
            u64::from(self.doc_size.x) * u64::from(self.doc_size.y),
            effect,
        )
    }

    /// Bake every stale effect and return the draw list the composite takes.
    ///
    /// `base` is the lowest slice number effects may use, and the caller owes it
    /// `LayerStack::slot_capacity_needed() + 1`. Both halves matter. It is one
    /// past everything the model has *claimed*, which includes a slice parked in
    /// an undo entry — `SlotPool` compacts only its tail, so `next` is above
    /// every parked slice as well as every live one, and an effect written below
    /// it would be an effect written over a deleted layer's pixels, found months
    /// later by an undo. The `+ 1` is the slice a floating transform previews
    /// into, taken at exactly `slot_capacity_needed()`.
    ///
    /// A `base` that would collide with something in `stack` is refused whole and
    /// logged: no effect is drawn and the plain draw list comes back. That is a
    /// caller error rather than a state, and drawing an effect over a layer is
    /// the one outcome nothing here may risk.
    ///
    /// # What it does not do
    ///
    /// It does not touch [`Self::slot_revision`]. An effect slice's pixels are
    /// derived, are never read back and are never captured into the undo history,
    /// so nothing invalidates against them — which is the same fact that lets an
    /// effect slice be freed rather than parked (§4.2).
    pub fn bake_effects(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        base: u32,
        stack: &[LayerEffects<'_>],
        frame: EffectFrame,
    ) -> BakedStack {
        // The draw list with no effects in it, and **the count of what that
        // cost**. The count is a parameter rather than zero: every route to this
        // list except the first is a refusal, and a refusal that reported nothing
        // dropped would be the silent degradation §6.1a forbids — a panel reading
        // the figure would say the document was within its budget while showing
        // none of it.
        let plain = |dropped: usize| BakedStack {
            draws: stack.iter().map(|e| e.draw).collect(),
            dropped,
            active_index: frame.active_index,
        };

        // Every effect the document would draw, bottom to top, as
        // (stack position, effect).
        //
        // **At most one per kind per layer, enforced here as well as upstream.**
        // `LayerStack::set_effect` guarantees it and `duplicate_effect_kind` is
        // what the reader asks, but this is a public function taking a bare
        // `&[Effect]`, and the failure a second one produces is not obvious: the
        // cache is keyed on `(slot, mask, kind)`, so both draws would read the
        // *second* effect's pixels while `record` kept one stamp — two entries in
        // the draw list showing one effect, rebaking every frame for ever. Cheaper
        // to drop the later one and say so than to make a caller's slip into that.
        let mut wanted: Vec<(usize, Effect)> = Vec::new();
        for (i, entry) in stack.iter().enumerate() {
            for effect in entry.effects.iter().filter(|e| !effect_marks_nothing(e)) {
                if wanted
                    .iter()
                    .any(|(at, e)| *at == i && e.kind == effect.kind)
                {
                    log::warn!("layer {i} carries two {:?} effects", effect.kind);
                    continue;
                }
                wanted.push((i, *effect));
            }
        }
        if wanted.is_empty() {
            // A document with no effects produces exactly the draw list it
            // produced before this feature existed, entry for entry — the
            // regression that matters most. Nothing is allocated and the scratch
            // is given back.
            self.effects.forget_all();
            return plain(0);
        }

        // A `base` below anything the stack itself uses is a caller that has read
        // the wrong number off the model. Refuse rather than overwrite: this is
        // the only failure here that damages a picture.
        let highest = stack
            .iter()
            .flat_map(|e| [Some(e.draw.slot), e.draw.mask].into_iter().flatten())
            .max()
            .unwrap_or(0);
        if base <= highest {
            log::error!("effect slices would start at {base}, over slot {highest} in use");
            self.effects.forget_all();
            self.effects.dropped = wanted.len();
            return plain(wanted.len());
        }

        // A slice count, and it is not the same bound as the byte budget: `127`
        // effect slices at 10000² would be 50 GB, which is stage 3's problem.
        // What this stops is the one that is fatal — a slice past the depth the
        // device guarantees. `MAX_SLOTS - base` can be **zero**: `SlotPool` hands
        // out up to slot 255, so `slot_capacity_needed` can reach 256 and `base`
        // 257, which enough delete-then-add cycles reach through parked slices.
        let capacity = MAX_EFFECT_SLICES.min(MAX_SLOTS.saturating_sub(base as usize));
        if capacity == 0 {
            // Nothing can be drawn, and it has to be said rather than fallen
            // through: below, `slots` would be empty and `top` would fall back to
            // `base` — 257, which is over the ceiling `ensure_slots` debug-asserts
            // and, in a release build, a fresh 256-slice array allocated and
            // copied every frame.
            log::warn!("no slices left for effects: they start at {base} of {MAX_SLOTS}");
            self.effects.forget_all();
            self.effects.dropped = wanted.len();
            return plain(wanted.len());
        }

        // Over budget: keep the effects **highest** in the stack, so the layer
        // somebody is working on keeps its own. Stated rather than truncated —
        // `dropped` is what the panel reports.
        //
        // **Two budgets, and the tighter of the two decides.** Slices are the one
        // §6.3 derives from the device; passes are this crate's own uniform
        // buffer, and at `MAX_ENABLED` effects the second bites first on a large
        // canvas. Routing both through one figure is what stops the pass budget
        // being a *silent* refusal — which it was, when overrunning it abandoned
        // the whole bake and left `dropped` reporting zero.
        let affordable = capacity.min(EFFECT_PASS_BLOCKS as usize / EFFECT_MAX_PASSES_PER_EFFECT);
        let dropped = wanted.len().saturating_sub(affordable);
        let kept = &wanted[dropped..];
        self.effects.dropped = dropped;
        if dropped > 0 {
            log::warn!(
                "document is over its effect budget: {dropped} of {} effects are not drawn",
                wanted.len()
            );
        }

        if self.effects.base != base {
            self.effects.forget_entries();
            self.effects.base = base;
        }

        // Which cache entry each kept effect is, allocating for the ones that are
        // new and releasing the ones nothing wants any more. A key is the *slot
        // the draw carries* and not the layer — §5.2 — so a float's preview slice
        // falls out of the cache rather than needing a rule.
        let keys: Vec<(u32, Option<u32>, EffectKind)> = kept
            .iter()
            .map(|(i, e)| (stack[*i].draw.slot, stack[*i].draw.mask, e.kind))
            .collect();
        self.effects.retain_only(&keys);
        let mut slots = Vec::with_capacity(kept.len());
        for key in &keys {
            match self.effects.slot_for(*key, capacity) {
                Some(slot) => slots.push(slot),
                None => {
                    // Unreachable: `capacity` bounds `kept` above. Named rather
                    // than unwrapped because the failure is a draw pointing at a
                    // slice nobody wrote.
                    log::error!("no effect slice for {key:?}");
                    self.effects.forget_all();
                    self.effects.dropped = wanted.len();
                    return plain(wanted.len());
                }
            }
        }

        // The array has to be deep enough for the highest slice a draw names
        // before any of them is written or read — **and no deeper**. Falling back
        // to `base` where nothing was assigned would pre-allocate the float's
        // spare slice for a document that has no float, and at `base == 257` it
        // asks for a slice past the ceiling: a `debug_assert` on the drawing path,
        // or in a release build a 256-slice array allocated and copied every
        // frame. `capacity == 0` is refused above, so this is belt and braces
        // rather than the only guard.
        if let Some(highest) = slots.iter().copied().max() {
            self.ensure_slots(device, queue, highest + 1);
        }

        // Plan the whole bake before recording any of it: every pass reads its
        // numbers out of one uniform buffer at submit time, so the blocks have to
        // be written before the first pass rather than as each is worked out.
        let mut steps: Vec<EffectStep> = Vec::new();
        // The layer whose coverage is in the scratch, **and whether the wet
        // stroke was folded into it**. Both halves, because the live gate is per
        // effect: a layer carrying a spread-0 shadow and a wide outline on a large
        // canvas has one effect that wants the scratch and one that does not, and
        // an extract shared between them would fold it in for both or for
        // neither. Keying on the pair means such a layer is extracted twice — one
        // more full-screen `R8Unorm` pass, which the pass budget already counts
        // per effect — and every effect reading a coverage records in its own
        // stamp the flag that coverage was built with.
        //
        // **Sharing one extract was a frozen half-stroke.** The extract took its
        // answer from the first stale effect on the layer, so the other one baked
        // from a coverage holding a partial wet stroke and stamped `live: false`;
        // `is_fresh` then held that entry for every later frame, and the mark sat
        // there until pointer-up moved the slice's revision. Silent, and only
        // reachable once the gate stopped being one value for the whole bake.
        let mut previous_source: Option<(u32, bool)> = None;
        for ((position, effect), slot) in kept.iter().zip(&slots) {
            let entry = &stack[*position];
            // **One binding, read by the stamp, the grouping key and the extract.**
            // Not three spellings of one expression: the property that has to hold
            // is that an effect records the flag its own coverage was built with,
            // and it holds here by construction rather than by the three agreeing.
            // It cannot be tested — a layer whose effects disagree needs a canvas
            // over `EFFECT_LIVE_PIXELS`, which is 4096 square — so structure is
            // the only guarantee available.
            //
            // Per effect, not per canvas: see `effect_bakes_live`.
            let wet = frame.stroke_live
                && frame.active_index as usize == *position
                && self.effect_bakes_live(effect);
            let stamp = CachedEffect {
                source: entry.draw.slot,
                mask: entry.draw.mask,
                kind: effect.kind,
                slot: *slot,
                source_revision: self.slot_revision(entry.draw.slot),
                mask_revision: entry.draw.mask.map_or(0, |m| self.slot_revision(m)),
                params: effect_params_hash(effect),
                live: wet,
            };
            if self.effects.is_fresh(&stamp) {
                continue;
            }
            // The coverage is per *layer and per live-ness*, so it is extracted
            // once for a layer whose effects agree about the wet stroke — which is
            // also why `kept` is walked in stack order rather than grouped.
            if previous_source != Some((entry.draw.slot, wet)) {
                steps.push(self.extract_step(entry, &frame, wet));
                previous_source = Some((entry.draw.slot, wet));
            }
            self.plan_effect(&mut steps, effect, *slot);
            self.effects.record(stamp);
        }
        if steps.is_empty() {
            return self.baked(stack, kept, &slots, frame);
        }

        if let Err(what) = self.run_effect_steps(device, queue, encoder, &steps) {
            // **The plain list, not the spliced one.** A bake that stopped part
            // way has left some of its slices unwritten, and a draw pointing at
            // one of those is whatever the driver left there — which is worse
            // than a picture with no shadow in it, and is exactly the failure
            // the over-budget rule refuses to produce silently. Every entry goes
            // with it, so the next frame rebakes from nothing rather than
            // trusting a stamp recorded for a pass that did not run.
            log::error!("effect bake abandoned: {what}");
            self.effects.forget_all();
            self.effects.dropped = wanted.len();
            return plain(wanted.len());
        }
        self.baked(stack, kept, &slots, frame)
    }

    /// The draw list, with each kept effect spliced in beside its layer.
    ///
    /// `docs/layer-effects.md` §4's order, and it falls out of two facts rather
    /// than being restated here: `Layer::effects` is held in composite order, and
    /// `Effect::is_outer` says which side of the layer each falls on.
    fn baked(
        &self,
        stack: &[LayerEffects<'_>],
        kept: &[(usize, Effect)],
        slots: &[u32],
        frame: EffectFrame,
    ) -> BakedStack {
        let mut draws = Vec::with_capacity(stack.len() + kept.len());
        let mut active_index = frame.active_index;
        let mut cursor = 0;
        for (position, entry) in stack.iter().enumerate() {
            let mine = |outer: bool| {
                kept.iter()
                    .zip(slots)
                    .filter(move |((p, e), _)| *p == position && e.is_outer() == outer)
            };
            for ((_, effect), slot) in mine(true) {
                draws.push(effect_draw(effect, *slot, entry));
                cursor += 1;
            }
            if frame.active_index as usize == position {
                active_index = cursor;
            }
            draws.push(entry.draw);
            cursor += 1;
            for ((_, effect), slot) in mine(false) {
                draws.push(effect_draw(effect, *slot, entry));
                cursor += 1;
            }
        }
        BakedStack {
            draws,
            dropped: self.effects.dropped,
            active_index,
        }
    }

    /// The pass that turns a layer slice into the coverage every effect on it
    /// derives from.
    fn extract_step(
        &self,
        entry: &LayerEffects<'_>,
        frame: &EffectFrame,
        stroke_here: bool,
    ) -> EffectStep {
        let size = self.doc_size;
        EffectStep {
            pass: EffectPass::Extract,
            target: EffectTarget::Coverage,
            bind: EffectBind::Extract as usize,
            viewport: size,
            cfg: EffectUniforms {
                size: [size.x as f32, size.y as f32],
                src_size: [size.x as f32, size.y as f32],
                slot: entry.draw.slot as i32,
                mask_slot: entry.draw.mask.unwrap_or(entry.draw.slot) as i32,
                has_mask: u32::from(entry.draw.mask.is_some()),
                stroke_here: u32::from(stroke_here),
                stroke_mode: match frame.stroke.mode {
                    BrushMode::Paint => 0,
                    BrushMode::Erase => 1,
                },
                stroke_opacity: frame.stroke.opacity.clamp(0.0, 1.0),
                stroke_on_mask: u32::from(frame.stroke.on_mask),
                stroke_gray: frame.stroke.color.r,
                ..EffectUniforms::default()
            },
        }
    }

    /// The grow, the soften, the displacement and the confinement, for one
    /// effect whose coverage is already in the scratch.
    fn plan_effect(&self, steps: &mut Vec<EffectStep>, effect: &Effect, slot: u32) {
        let size = self.doc_size;
        let full = [size.x as f32, size.y as f32];
        let plan = effect_field(effect);
        let base = EffectUniforms {
            size: full,
            src_size: full,
            spread: plan.reach,
            shape: plan.shape as u32,
            invert: u32::from(plan.invert),
            ..EffectUniforms::default()
        };

        // The distance field, and only where something needs one. A drop shadow
        // with no spread is a blur of the coverage and nothing else, which is the
        // setting every application opens one at — so the flood, which is the
        // expensive half of this whole feature, is skipped for the common case.
        let grow = plan.reach > 0.0;
        if grow {
            // A centred outline is the one position that floods **twice**: its
            // band straddles the edge, so it needs the outward distance and the
            // inward one, and one ping-pong pair cannot hold both. The outward
            // half goes into the band plane first, unconfined, and the inward
            // pass then combines the two. `plan.shape` is `Centre` for the second
            // grow; the first is `Raw`.
            //
            // **An exhaustive `match` rather than `== EffectShape::Centre`**, for
            // the reason CLAUDE.md's "Partial exhaustiveness" section gives: an
            // equality test is the `matches!` hazard wearing a different operator,
            // so a sixth shape that also needed two fields would silently get one
            // and read a band plane nothing had written. Written as the *count* of
            // fields, which is the thing the planner actually needs to know.
            let fields = match plan.shape {
                EffectShape::Centre => 2,
                EffectShape::Dilate
                | EffectShape::Outer
                | EffectShape::Inner
                | EffectShape::Raw => 1,
            };
            if fields == 2 {
                self.plan_field(
                    steps,
                    &EffectUniforms {
                        shape: EffectShape::Raw as u32,
                        invert: 0,
                        ..base
                    },
                    EffectTarget::Band,
                    false,
                );
                self.plan_field(steps, &base, EffectTarget::Grown, true);
            } else {
                self.plan_field(steps, &base, EffectTarget::Grown, false);
            }
        }
        // With nothing to grow, the shape **is** the coverage and no grow pass is
        // recorded: `fs_grow` with `grow == 0` hands the coverage straight back, so
        // the pass was a full-screen copy of a texture already in hand. That is the
        // common case — a drop shadow's default spread is zero, and its
        // displacement is what makes it a shadow. The bind groups below are what
        // pay for it, and they are what a placeholder cannot be: the shape's source
        // is a *binding*, so choosing it is choosing a bind group.
        let shape = if grow {
            (
                EffectBind::SrcGrown as usize,
                EffectBind::ResolveGrown as usize,
            )
        } else {
            (
                EffectBind::SrcCoverage as usize,
                EffectBind::ResolveCoverage as usize,
            )
        };
        self.plan_soften_and_resolve(steps, effect, &base, shape, slot);
    }

    /// One jump flood and the grow pass that reads it.
    ///
    /// `combine` runs the grow through the bind groups that hold the band plane,
    /// which is what a centred outline's *second* field needs and nothing else
    /// does.
    fn plan_field(
        &self,
        steps: &mut Vec<EffectStep>,
        base: &EffectUniforms,
        target: EffectTarget,
        combine: bool,
    ) {
        let size = self.doc_size;
        let reach = base.spread;
        {
            steps.push(EffectStep {
                pass: EffectPass::Seed,
                target: EffectTarget::Seed(0),
                bind: EffectBind::Coverage as usize,
                viewport: size,
                cfg: *base,
            });
            // `ceil(log2(reach)) + 1` halving steps, largest first. One more than
            // the log because the last step at k = 1 is what settles a
            // neighbouring texel, and the flood is only exact once it has run.
            //
            // **Bounded by the canvas, and that is not tidiness.** A spread wider
            // than the longest side reaches every texel already, so more steps
            // buy nothing — and unbounded this is a shift by `32 - leading_zeros`
            // of a saturating `as u32`, which at a spread near four billion is a
            // shift of 32 and therefore a panic on the drawing path. `spread` is
            // an `f32` a file carries, so "nobody would type that" is not a
            // bound. `max(1.0)` before the `min` also disposes of a NaN, since
            // `f32::max` answers the operand that is not one.
            let longest = size.x.max(size.y) as f32;
            let span = reach.ceil().max(1.0).min(longest) as u32;
            let mut from = 0usize;
            let mut k = 1i32 << (32 - span.leading_zeros());
            while k >= 1 {
                steps.push(EffectStep {
                    pass: EffectPass::Flood,
                    target: EffectTarget::Seed(1 - from),
                    bind: if from == 0 {
                        EffectBind::Flood0 as usize
                    } else {
                        EffectBind::Flood1 as usize
                    },
                    viewport: size,
                    cfg: EffectUniforms { k, ..*base },
                });
                from = 1 - from;
                k /= 2;
            }
            steps.push(EffectStep {
                pass: EffectPass::Grow,
                target,
                bind: match (combine, from) {
                    (false, 0) => EffectBind::Grow0 as usize,
                    (false, _) => EffectBind::Grow1 as usize,
                    (true, 0) => EffectBind::CombineSeed0 as usize,
                    (true, _) => EffectBind::CombineSeed1 as usize,
                },
                viewport: size,
                cfg: EffectUniforms { grow: 1, ..*base },
            });
        }
    }

    /// The tent, the displacement, the tint and the knockout.
    fn plan_soften_and_resolve(
        &self,
        steps: &mut Vec<EffectStep>,
        effect: &Effect,
        base: &EffectUniforms,
        shape: (usize, usize),
        slot: u32,
    ) {
        let size = self.doc_size;
        let full = [size.x as f32, size.y as f32];
        let base = *base;

        // The tent: two box passes per axis, on a downsample where the radius is
        // wide enough for one to represent it. A radius of zero records no pass
        // at all, which is the exact identity the feather and the grain both
        // keep — the shape itself is what the resolve then reads.
        let (down, radius) = tent_for(effect.softness);
        let mut blurred = None;
        if radius > 0 {
            let small = UVec2::new(size.x.div_ceil(down), size.y.div_ceil(down));
            let small_f = [small.x as f32, small.y as f32];
            let mut from = 0usize;
            if down > 1 {
                steps.push(EffectStep {
                    pass: EffectPass::Down,
                    target: EffectTarget::Blur(0),
                    bind: shape.0,
                    viewport: small,
                    cfg: EffectUniforms {
                        size: small_f,
                        src_size: full,
                        down: down as i32,
                        ..base
                    },
                });
                from = 0;
            }
            // Four box passes, two per axis: a tent is the box convolved with
            // itself and convolution is per axis. The first reads the shape when
            // there was no downsample, which is what makes the two resolutions one
            // code path rather than two.
            for (i, axis) in [[1.0, 0.0], [1.0, 0.0], [0.0, 1.0], [0.0, 1.0]]
                .into_iter()
                .enumerate()
            {
                let first = i == 0 && down == 1;
                let bind = if first {
                    shape.0
                } else if from == 0 {
                    EffectBind::SrcBlur0 as usize
                } else {
                    EffectBind::SrcBlur1 as usize
                };
                let to = if first { 0 } else { 1 - from };
                steps.push(EffectStep {
                    pass: EffectPass::Box,
                    target: EffectTarget::Blur(to),
                    bind,
                    viewport: small,
                    cfg: EffectUniforms {
                        size: small_f,
                        src_size: small_f,
                        radius,
                        step: axis,
                        ..base
                    },
                });
                from = to;
            }
            blurred = Some((from, small_f));
        }

        let (bind, src_size, read_down) = match blurred {
            Some((i, small)) => (
                if i == 0 {
                    EffectBind::ResolveBlur0 as usize
                } else {
                    EffectBind::ResolveBlur1 as usize
                },
                small,
                down as i32,
            ),
            None => (shape.1, full, 1),
        };
        let (dx, dy) = effect.offset();
        let c = effect.color;
        steps.push(EffectStep {
            pass: EffectPass::Resolve,
            target: EffectTarget::Slice(slot),
            bind,
            viewport: size,
            cfg: EffectUniforms {
                // Premultiplied, with alpha 1: the coverage the shader works out
                // scales all four channels together. The colour is **linear**,
                // and the target is an sRGB view, so the hardware encodes it
                // exactly as it encodes a layer's own pixels.
                tint: [c.r, c.g, c.b, 1.0],
                size: full,
                src_size,
                offset: [dx, dy],
                down: read_down,
                // **The knockout is the drop shadow's alone.** An outline's
                // confinement happened in the grow, where it belongs: it is what
                // "outside the edge" or "inside it" means, and it has to be
                // applied before the blur so a soft stroke is soft on both sides.
                // The shadow's has to be applied *after* the displacement,
                // because what it must not cover is where the layer is now.
                knockout: u32::from(effect.kind == EffectKind::DropShadow),
                ..EffectUniforms::default()
            },
        });
    }

    /// Write every planned pass's uniform block, then record every pass.
    fn run_effect_steps(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        steps: &[EffectStep],
    ) -> Result<(), String> {
        // Unreachable: `bake_effects` bounds the number of effects it plans by
        // this buffer's capacity, and drops the rest through the *visible* path.
        // Kept because what it prevents is a dynamic offset past the end of a
        // buffer, which is a validation error and therefore fatal — and because
        // this used to be the only bound, which made an overrun a silent refusal
        // of the whole bake with `dropped` reporting zero.
        if steps.len() as u64 > EFFECT_PASS_BLOCKS {
            return Err(format!(
                "{} passes against {EFFECT_PASS_BLOCKS} uniform blocks",
                steps.len()
            ));
        }
        let needs_seeds = steps
            .iter()
            .any(|s| matches!(s.pass, EffectPass::Seed | EffectPass::Flood));
        let needs_band = steps.iter().any(|s| matches!(s.target, EffectTarget::Band));
        self.ensure_effect_scratch(device, needs_seeds, needs_band);
        let Some(scratch) = self.effects.scratch.as_ref() else {
            return Err("no working set".into());
        };
        if needs_seeds && scratch.seeds.is_none() {
            return Err("no seed pair".into());
        }
        if needs_band && scratch.band.is_none() {
            return Err("no band plane".into());
        }

        for (i, step) in steps.iter().enumerate() {
            queue.write_buffer(
                &scratch.uniforms,
                i as u64 * EFFECT_BLOCK_STRIDE,
                bytemuck::bytes_of(&step.cfg),
            );
        }

        for (i, step) in steps.iter().enumerate() {
            let target = match step.target {
                EffectTarget::Coverage => &scratch.coverage,
                EffectTarget::Grown => &scratch.grown,
                EffectTarget::Band => match scratch.band.as_ref() {
                    Some(view) => view,
                    None => return Err("a centred outline with no band plane".into()),
                },
                EffectTarget::Blur(n) => &scratch.blur[n],
                EffectTarget::Seed(n) => match scratch.seeds.as_ref() {
                    Some(pair) => &pair[n],
                    None => return Err("a flood with no seed pair".into()),
                },
                EffectTarget::Slice(slot) => match self.layers.slot_views.get(slot as usize) {
                    Some(view) => view,
                    None => return Err(format!("effect slice {slot} beyond capacity")),
                },
            };
            let pipeline = match step.pass {
                EffectPass::Extract => &self.shared.effect_extract,
                EffectPass::Seed => &self.shared.effect_seed,
                EffectPass::Flood => &self.shared.effect_step,
                EffectPass::Grow => &self.shared.effect_grow,
                EffectPass::Down => &self.shared.effect_down,
                EffectPass::Box => &self.shared.effect_box,
                EffectPass::Resolve => &self.shared.effect_resolve,
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("effect-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    resolve_target: None,
                    depth_slice: None,
                    // Every texel of the viewport is written, so loading whatever
                    // was there would only be a dependency the driver has to
                    // honour. On the blur pair the viewport is smaller than the
                    // attachment and the rest is cleared, which costs nothing and
                    // keeps a stale radius from being read back through a clamp.
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_viewport(
                0.0,
                0.0,
                step.viewport.x as f32,
                step.viewport.y as f32,
                0.0,
                1.0,
            );
            pass.set_pipeline(pipeline);
            pass.set_bind_group(
                0,
                &scratch.binds[step.bind],
                &[(i as u64 * EFFECT_BLOCK_STRIDE) as u32],
            );
            pass.draw(0..3, 0..1);
        }
        self.effects.bakes += 1;
        Ok(())
    }

    /// Build the canvas-sized working set if it is missing or the wrong shape.
    fn ensure_effect_scratch(&mut self, device: &wgpu::Device, seeds: bool, band: bool) {
        let stale = match self.effects.scratch.as_ref() {
            None => true,
            Some(s) => {
                s.size != self.doc_size
                    || s.bound_capacity != self.layers.capacity
                    || (seeds && s.seeds.is_none())
                    || (band && s.band.is_none())
            }
        };
        if !stale {
            return;
        }
        // Keeping the lazy planes once they have been allocated: an effect whose
        // spread is being dragged crosses zero repeatedly, and reallocating
        // 800 MB at 10000² on the way past would be worse than holding it. The
        // same for the band plane, which switching an outline between Centre and
        // Outside would otherwise take and give back on alternate frames.
        let had = self.effects.scratch.as_ref();
        let keep_seeds = seeds || had.is_some_and(|s| s.seeds.is_some());
        let keep_band = band || had.is_some_and(|s| s.band.is_some());
        self.effects.scratch = Some(EffectScratch::new(
            device,
            &self.shared,
            self.doc_size,
            &self.layers,
            &self.stroke_view,
            keep_seeds,
            keep_band,
        ));
    }

    // --- the whole-document capture ----------------------------------------

    /// True while a capture is in flight, abandoned or otherwise.
    ///
    /// Abandoned counts: the staging buffer is still the GPU's until its map
    /// settles, so a second capture would allocate beside it rather than
    /// instead of it.
    pub fn capture_in_flight(&self) -> bool {
        self.capture.is_some()
    }

    /// Start reading the whole document back, without blocking.
    ///
    /// `slots` are the texture-array slices to read, in stack order; `draws` is
    /// the same stack the composite pass takes, for the flattened preview.
    /// Returns false when one is already in flight — the caller's cue to try
    /// again later rather than to queue a second.
    ///
    /// Nothing is copied here. [`Self::drive_capture`] records one step,
    /// [`Self::submit_capture`] maps it, and [`Self::take_capture`] collects it
    /// and lets the next step go. See [`Capture`] for why it is spread out.
    pub fn begin_capture(&mut self, slots: &[u32], draws: &[LayerDraw]) -> bool {
        if self.capture.is_some() || slots.is_empty() {
            return false;
        }
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = (self.doc_size.x * 4).div_ceil(align) * align;
        self.capture = Some(Capture {
            size: self.doc_size,
            padded,
            slots: slots.to_vec(),
            draws: draws.to_vec(),
            buffer: None,
            band: 0,
            row: 0,
            outcome: Arc::new(AtomicU8::new(PROBE_PENDING)),
            state: StepState::Waiting,
            step: 0,
            // One per layer, and one for the flattened preview the format
            // requires.
            results: (0..slots.len() + 1).map(|_| None).collect(),
            partial: None,
            merged_target: None,
            abandoned: false,
            failed: false,
        });
        true
    }

    /// Record the next step's copy into this frame's encoder, if the staging
    /// buffer is free.
    ///
    /// Costs one `copy_texture_to_buffer` — or, for the last step, one
    /// composite into an offscreen target and then the copy. Both are
    /// *recorded*, not executed: nothing on this path waits for the GPU.
    pub fn drive_capture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        // Taken out because the preview's composite borrows `self` — putting it
        // back is unconditional, so the job cannot be lost down any path.
        let Some(mut job) = self.capture.take() else {
            return;
        };
        if job.abandoned || job.failed || job.state != StepState::Waiting || job.complete() {
            self.capture = Some(job);
            return;
        }

        let index = job.step;
        job.band = band_rows(self.readback_limit, job.padded, job.size.y);
        let (band_first, band_last) = job.band_span();
        let height = band_last as u32 - band_first as u32;
        // Allocated once and reused for every band of every step. A buffer per
        // layer would be the document's own size in staging memory on top of the
        // copy of it being assembled.
        let buffer = job.buffer.take().unwrap_or_else(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("umber-capture"),
                size: (job.padded as u64) * (job.band as u64),
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        });

        let source = if index < job.slots.len() {
            let slot = job.slots[index];
            if slot >= self.layers.capacity {
                // Cannot happen — `ensure_slots` runs before a layer is ever
                // painted — but a copy naming a slice the array does not have
                // is a validation error, and a validation error aborts the
                // process. An autosave is not worth taking the app down for.
                log::error!("capture named slot {slot} beyond capacity");
                job.failed = true;
                job.buffer = Some(buffer);
                self.capture = Some(job);
                return;
            }
            wgpu::TexelCopyTextureInfo {
                texture: &self.layers.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: band_first as u32,
                    z: slot,
                },
                aspect: wgpu::TextureAspect::All,
            }
        } else {
            // The flattened preview, from the *same* composite pass the screen
            // uses — the reason `export_rgba` and `pick_colour` reuse it too.
            // A second copy of the blend maths here would be a second thing to
            // keep in step, and a preview that disagreed with the screen is the
            // bug that arrangement produces.
            //
            // Drawn once per *step*, not once per band: the later bands of a
            // banded capture read further down the same flattened image, so
            // re-compositing would be the whole document's blend maths run again
            // to fetch rows that are already sitting there.
            if job.merged_target.is_none() {
                let target = self.export_target(device);
                let view = target.create_view(&wgpu::TextureViewDescriptor::default());
                self.render_export(queue, encoder, &view, &job.draws);
                job.merged_target = Some(target);
            }
            wgpu::TexelCopyTextureInfo {
                // Held in `job.merged_target` for the rest of this function.
                texture: job.merged_target.as_ref().expect("just set"),
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: band_first as u32,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            }
        };

        encoder.copy_texture_to_buffer(
            source,
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(job.padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width: job.size.x,
                height,
                depth_or_array_layers: 1,
            },
        );

        job.buffer = Some(buffer);
        job.state = StepState::Rendering;
        self.capture = Some(job);
    }

    /// Map whatever [`Self::drive_capture`] recorded, once the frame holding it
    /// has been submitted.
    ///
    /// Separate from recording for the same reason [`Self::submit_probes`] is:
    /// `map_async` on a buffer whose copy has not been submitted would map it
    /// before the GPU has written to it.
    pub fn submit_capture(&mut self) {
        let Some(job) = self.capture.as_mut() else {
            return;
        };
        if job.state != StepState::Rendering {
            return;
        }
        let Some(buffer) = job.buffer.as_ref() else {
            return;
        };
        job.state = StepState::Mapping;
        job.outcome.store(PROBE_PENDING, Ordering::Release);
        let outcome = job.outcome.clone();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                // Recorded rather than ignored, for the reason the probe's
                // callback records it: a failed map leaves the buffer unmapped,
                // and unmapping one that was never mapped is itself an error.
                let code = if result.is_ok() {
                    PROBE_MAPPED
                } else {
                    PROBE_FAILED
                };
                outcome.store(code, Ordering::Release);
            });
        // The preview's offscreen target is dropped by `take_capture` once the
        // last band has been copied out of it; holding it any longer would keep
        // a canvas-sized texture alive for the rest of the readback. It cannot
        // go here any more, because a banded capture comes back for the rows
        // below this one.
    }

    /// Collect the step that has come home, and hand back the document once the
    /// last of them has.
    ///
    /// Polls without blocking, like [`Self::take_probe`]. At most one layer's
    /// worth of bytes is copied out per call, which is what keeps the cost of
    /// an autosave to one memcpy in the frames that have one at all.
    pub fn take_capture(&mut self, device: &wgpu::Device) -> Option<DocumentCapture> {
        self.capture.as_ref()?;
        let _ = device.poll(wgpu::PollType::Poll);
        let job = self.capture.as_mut()?;

        if job.state == StepState::Mapping {
            match job.outcome.load(Ordering::Acquire) {
                PROBE_MAPPED => {
                    // A capture that has been abandoned still has to unmap, but
                    // there is no point reading sixteen megabytes out of it.
                    let dropped = job.abandoned || job.failed;
                    let band_done = if dropped {
                        job.partial = None;
                        true
                    } else {
                        job.copy_chunk()
                    };
                    if band_done {
                        job.buffer
                            .as_ref()
                            .expect("a mapped step has its buffer")
                            .unmap();
                        // A step is one band on any ordinary document and
                        // several on one too large for the device's staging
                        // limit. Only when the last of them is out does the
                        // layer count as read.
                        if !dropped {
                            job.row += job.band;
                            if job.step_done() {
                                job.row = 0;
                                if let Some(bytes) = job.partial.take() {
                                    job.results[job.step] = Some(bytes);
                                    job.step += 1;
                                }
                                // The flattened preview has been read out of;
                                // give the canvas-sized texture back.
                                job.merged_target = None;
                            }
                        }
                        job.state = StepState::Waiting;
                    }
                }
                // Nothing to read and nothing to unmap. The whole capture goes:
                // a document missing one layer is not a shorter document, it is
                // a wrong one.
                PROBE_FAILED => {
                    job.failed = true;
                    job.state = StepState::Waiting;
                }
                // Still in flight. Leaving it alone is the whole point.
                _ => {}
            }
        }

        if job.abandoned || job.failed {
            if job.settled() {
                if job.failed {
                    log::warn!("a document capture could not be read back; nothing was written");
                }
                self.capture = None;
            }
            return None;
        }
        if !job.complete() {
            return None;
        }

        let job = self.capture.take().expect("checked above");
        let size = job.size;
        let mut buffers: Vec<Vec<u8>> = job
            .results
            .into_iter()
            .map(|r| r.expect("a complete capture has every buffer"))
            .collect();
        let merged = buffers.pop().expect("the preview is the last step");
        Some(DocumentCapture {
            size,
            layers: buffers,
            merged,
        })
    }

    /// Disown a capture in flight, because what it is reading is about to stop
    /// being true — a resize, or the document being closed.
    ///
    /// Note what this does *not* do: free the buffer. A `map_async` that is
    /// still outstanding makes its buffer untouchable, and dropping the job
    /// here is the same class of mistake [`Self::reset_probes`] documents. The
    /// job stays until [`Self::take_capture`] finds it settled.
    pub fn cancel_capture(&mut self) {
        if let Some(job) = self.capture.as_mut() {
            job.abandoned = true;
        }
    }

    /// Pretend this device will not allocate a staging buffer larger than
    /// `bytes`, so the banded readback path can be driven on a document small
    /// enough to check by hand.
    ///
    /// Exists for the tests. Reaching the real limit takes a canvas of about
    /// 8192², which is more memory than a test should ask a CI runner for — and
    /// an untested path that only the largest documents take is the one that
    /// silently returns a sheared picture.
    pub fn set_readback_limit(&mut self, bytes: u64) {
        self.readback_limit = bytes;
    }

    /// Read a rectangle of one layer back to the CPU, for the undo stack.
    ///
    /// This blocks until the GPU catches up. That is acceptable because it runs
    /// once per stroke at pointer-up, never inside the drawing loop. An
    /// autosave must not use it — see [`Capture`].
    pub fn read_layer_rect(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        slot: u32,
        rect: PixelRect,
    ) -> Vec<u8> {
        // A copy naming a slice the array does not have is a validation error,
        // and a validation error aborts the process. It should not happen —
        // `ensure_slots` runs before a layer is ever painted — but "should not"
        // is not a reason to make it fatal, and the resume path rebuilds
        // storage from scratch with the stack already deep. An all-zero patch
        // is the same thing an untouched layer would have read back.
        if slot >= self.layers.capacity {
            log::error!(
                "read from slot {slot} beyond capacity {}",
                self.layers.capacity
            );
            return vec![0; (rect.width as u64 * 4 * rect.height as u64) as usize];
        }
        self.read_texture_rows(
            device,
            queue,
            "undo",
            wgpu::TexelCopyTextureInfo {
                texture: &self.layers.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: rect.x,
                    y: rect.y,
                    z: slot,
                },
                aspect: wgpu::TextureAspect::All,
            },
            (rect.width, rect.height),
        )
    }

    /// Read several rectangles of one layer back to the CPU, for the undo
    /// stack, in **one** submission and one wait.
    ///
    /// This is what a stroke's patch is captured with. The pieces are the cells
    /// of the canvas the stroke actually reached
    /// ([`umber_core::damage::TileMask`]), which for a diagonal across a large
    /// document is a hundred and fifty separate rectangles — and a hundred and
    /// fifty calls to [`Self::read_layer_rect`] would be a hundred and fifty
    /// submissions each blocking on its own fence, at pointer-up, in front of
    /// the artist. Recorded together they cost one.
    ///
    /// Blocking, like [`Self::read_layer_rect`] and for the same reason: once
    /// per stroke is acceptable, the drawing loop is not.
    pub fn read_layer_pieces(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        slot: u32,
        pieces: &[PixelRect],
    ) -> Vec<Vec<u8>> {
        // As in `read_layer_rect`: refuse rather than abort. See there.
        if slot >= self.layers.capacity {
            log::error!(
                "read from slot {slot} beyond capacity {}",
                self.layers.capacity
            );
            return pieces
                .iter()
                .map(|r| vec![0; (r.area() * 4) as usize])
                .collect();
        }

        let align = u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        // Every block is a whole number of padded rows, so laying them end to
        // end keeps each one's offset aligned without any arithmetic of its
        // own.
        let block =
            |r: &PixelRect| (u64::from(r.width) * 4).div_ceil(align) * align * u64::from(r.height);

        let mut out: Vec<Vec<u8>> = Vec::with_capacity(pieces.len());
        let mut batch: Vec<&PixelRect> = Vec::new();
        let mut used = 0u64;
        let flush = |batch: &mut Vec<&PixelRect>, out: &mut Vec<Vec<u8>>| {
            if !batch.is_empty() {
                self.read_batch(device, queue, slot, batch, out);
                batch.clear();
            }
        };

        for piece in pieces {
            let size = block(piece);
            // One piece larger than the whole limit cannot be batched at all.
            // Cell runs are at most one cell tall, so this needs a patch that
            // did not come from a damage mask — but the banded path exists and
            // costs nothing to fall back to.
            if size > self.readback_limit {
                flush(&mut batch, &mut out);
                out.push(self.read_layer_rect(device, queue, slot, *piece));
                continue;
            }
            if used + size > self.readback_limit {
                flush(&mut batch, &mut out);
                used = 0;
            }
            used += size;
            batch.push(piece);
        }
        flush(&mut batch, &mut out);
        out
    }

    /// One submission's worth of [`Self::read_layer_pieces`]: every piece
    /// copied into one staging buffer, then mapped once.
    fn read_batch(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        slot: u32,
        pieces: &[&PixelRect],
        out: &mut Vec<Vec<u8>>,
    ) {
        let align = u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let padded = |r: &PixelRect| (u64::from(r.width) * 4).div_ceil(align) * align;

        let mut offsets = Vec::with_capacity(pieces.len());
        let mut size = 0u64;
        for piece in pieces {
            offsets.push(size);
            size += padded(piece) * u64::from(piece.height);
        }

        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("undo-pieces-readback"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("undo-pieces"),
        });
        for (piece, offset) in pieces.iter().zip(&offsets) {
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.layers.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: piece.x,
                        y: piece.y,
                        z: slot,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &staging,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: *offset,
                        bytes_per_row: Some(padded(piece) as u32),
                        rows_per_image: Some(piece.height),
                    },
                },
                wgpu::Extent3d {
                    width: piece.width,
                    height: piece.height,
                    depth_or_array_layers: 1,
                },
            );
        }
        queue.submit(Some(encoder.finish()));

        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        let mapped = slice.get_mapped_range();
        for (piece, offset) in pieces.iter().zip(&offsets) {
            let unpadded = (piece.width * 4) as usize;
            let padded = padded(piece) as usize;
            let mut bytes = Vec::with_capacity(unpadded * piece.height as usize);
            for row in 0..piece.height as usize {
                let start = *offset as usize + row * padded;
                bytes.extend_from_slice(&mapped[start..start + unpadded]);
            }
            out.push(bytes);
        }
        drop(mapped);
        staging.unmap();
    }

    /// Copy a rectangle of a texture back to the CPU, blocking, and return it
    /// tightly packed — the 256-byte row padding the copy requires stripped.
    ///
    /// Goes a band of rows at a time, because a document can be larger than the
    /// largest buffer the device will allocate; see [`band_rows`]. One buffer is
    /// made and reused for every band, so the banded path costs extra submits
    /// and nothing else. Both blocking readbacks share this: a second copy of
    /// the padding arithmetic is a second place for a document to come back
    /// sheared.
    fn read_texture_rows(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &str,
        source: wgpu::TexelCopyTextureInfo<'_>,
        size: (u32, u32),
    ) -> Vec<u8> {
        let (width, height) = size;
        let unpadded = width * 4;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;
        let band = band_rows(self.readback_limit, padded, height);

        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(&format!("{label}-readback")),
            size: (padded as u64) * (band as u64),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut out = Vec::with_capacity((unpadded as usize) * (height as usize));
        let mut first = 0;
        while first < height {
            let rows = band.min(height - first);
            let mut encoder = device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    origin: wgpu::Origin3d {
                        y: source.origin.y + first,
                        ..source.origin
                    },
                    ..source
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &staging,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(padded),
                        rows_per_image: Some(rows),
                    },
                },
                wgpu::Extent3d {
                    width,
                    height: rows,
                    depth_or_array_layers: 1,
                },
            );
            queue.submit(Some(encoder.finish()));

            // Only the rows this band wrote: the buffer is a whole band long and
            // the last one is usually short, so mapping all of it would append
            // whatever the previous band left behind.
            let slice = staging.slice(..(padded as u64) * (rows as u64));
            slice.map_async(wgpu::MapMode::Read, |_| {});
            let _ = device.poll(wgpu::PollType::wait_indefinitely());

            let mapped = slice.get_mapped_range();
            for row in 0..rows {
                let start = (row * padded) as usize;
                out.extend_from_slice(&mapped[start..start + unpadded as usize]);
            }
            drop(mapped);
            staging.unmap();
            first += rows;
        }
        out
    }

    /// Write a previously captured rectangle back into one layer.
    ///
    /// **Goes a band of rows at a time, for the reason every readback here
    /// does** — see [`band_rows`]. `Queue::write_texture` allocates a staging
    /// buffer the size of the upload and copies the caller's bytes into it, so
    /// a canvas-sized write asks the device for a canvas-sized buffer: 400 MB
    /// on a 10000² document, well past the 256 MB `downlevel_defaults` says a
    /// buffer may be. The reader was banded for exactly that and the writer was
    /// not, which is the asymmetry this closes. `band_rows` returns the whole
    /// rectangle whenever it fits, so an ordinary document takes the same
    /// single `write_texture` it always did.
    ///
    /// **A staging buffer is not released at submit, it is released on that
    /// submission's fence**, so it is not enough to submit between bands: the
    /// bytes stand until the GPU has consumed them. wgpu triages finished
    /// submissions with a non-blocking poll at the end of every `submit`, so a
    /// GPU keeping up retires each band as the next is written — but that is a
    /// statement about a machine, not a bound. Where the rectangle actually had
    /// to be banded this therefore **waits** for each band, which makes the
    /// staging held at any instant exactly one band. That is a blocking call
    /// and it is confined to the case that earns it: a slice too large for one
    /// buffer, which today is an import or an undo on a very large canvas, both
    /// of them paths where the artist is already waiting. Where nothing was
    /// banded nothing waits.
    ///
    /// Why it matters more than the megabytes suggest: `StagingBuffer::new`
    /// reports a failed allocation through wgpu's *fatal* `handle_hal_error`,
    /// which no error scope can catch and which loses the device outright. What
    /// is being bounded is whether the document opens at all.
    ///
    /// **A loop of these still accumulates, and the caller has to know it.**
    /// The submits here bound one call; several calls with nothing between them
    /// hold every one of their last bands until the GPU catches up. `App::
    /// install_import` is the loop that made this visible — twenty-one layers
    /// of a hundred-megapixel document held 8.4 GB of staging on top of an
    /// 8.4 GB layer array — and `App::swap_patch` is a second one, bounded by
    /// the undo budget rather than by a layer count. Anybody writing a third
    /// should assume the same.
    pub fn write_layer_rect(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        slot: u32,
        rect: PixelRect,
        bytes: &[u8],
    ) {
        debug_assert_eq!(bytes.len() as u64, rect.area() * 4);
        self.touch_slot(slot);
        // As in `read_layer_rect`: refuse rather than abort. See there.
        if slot >= self.layers.capacity {
            log::error!(
                "write to slot {slot} beyond capacity {}",
                self.layers.capacity
            );
            return;
        }
        // The importers promise canvas-sized pixels and the undo stack promises
        // rect-sized ones, but both come from files, and a short buffer here is
        // a validation error rather than a wrong picture.
        if (bytes.len() as u64) < rect.area() * 4 {
            log::error!(
                "layer write has {} bytes for a {}x{} rect",
                bytes.len(),
                rect.width,
                rect.height
            );
            return;
        }

        // The staging cost is the *padded* row, because wgpu repacks the
        // caller's tightly packed rows to the copy alignment on the way in.
        // `bytes` itself is tight, which is what `stride` below steps by.
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = (rect.width * 4).div_ceil(align) * align;
        let band = band_rows(self.readback_limit, padded, rect.height);
        let banded = band < rect.height;

        let stride = (rect.width as usize) * 4;
        let mut first = 0;
        while first < rect.height {
            let rows = band.min(rect.height - first);
            let start = (first as usize) * stride;
            write_rect(
                queue,
                &self.layers.texture,
                wgpu::Origin3d {
                    x: rect.x,
                    y: rect.y + first,
                    z: slot,
                },
                PixelRect {
                    x: rect.x,
                    y: rect.y + first,
                    width: rect.width,
                    height: rows,
                },
                &bytes[start..start + (rows as usize) * stride],
            );
            // Flushes this band's staging into a submission of its own. On the
            // unbanded path this is the one call, and it is what stops a loop
            // of writes holding every layer's staging at once.
            queue.submit([]);
            if banded {
                let _ = device.poll(wgpu::PollType::wait_indefinitely());
            }
            first += rows;
        }
    }
}

/// The smallest rectangle covering both, where either may be absent.
///
/// What a preview has to restore: the rectangle it wrote last time and the one
/// it is about to write. Missing either leaves a trail of the drag behind on
/// the canvas.
fn span(a: Option<PixelRect>, b: Option<PixelRect>) -> Option<PixelRect> {
    match (a, b) {
        (Some(a), Some(b)) => Some(transform::union(a, b)),
        (a, b) => a.or(b),
    }
}

/// Upload tightly packed RGBA8 into a rectangle of a texture.
fn write_rect(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    origin: wgpu::Origin3d,
    rect: PixelRect,
    bytes: &[u8],
) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin,
            aspect: wgpu::TextureAspect::All,
        },
        bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(rect.width * 4),
            rows_per_image: Some(rect.height),
        },
        wgpu::Extent3d {
            width: rect.width,
            height: rect.height,
            depth_or_array_layers: 1,
        },
    );
}

fn mode_index(mode: BrushMode) -> u32 {
    match mode {
        BrushMode::Paint => 0,
        BrushMode::Erase => 1,
    }
}

fn clear_view(encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, label: &str) {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            depth_slice: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
}

/// Upload an 8-bit mask — a tip or a paper tile — into a fresh texture.
fn upload_mask(device: &wgpu::Device, queue: &wgpu::Queue, mask: &TipMask) -> wgpu::Texture {
    upload_coverage(
        device,
        queue,
        mask.width(),
        mask.height(),
        mask.coverage(),
        "umber-brush-tip",
    )
}

/// Put `bytes` — one per texel, row-major — into a new coverage texture.
fn upload_coverage(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    width: u32,
    height: u32,
    bytes: &[u8],
    label: &str,
) -> wgpu::Texture {
    let texture = make_coverage_texture(device, width, height, label);
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            // One byte per texel: R8Unorm is all a coverage mask needs, matching
            // the stroke scratch it feeds.
            bytes_per_row: Some(width),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    texture
}

/// Storage for a brush tip: single-channel coverage, matching the stroke
/// scratch it feeds. Four channels would be four times the bandwidth to say the
/// same thing.
fn make_tip_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    make_coverage_texture(device, width, height, "umber-brush-tip")
}

/// Storage for a coloured stamp's colour. See [`TIP_COLOR_FORMAT`].
fn make_tip_color_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("umber-brush-tip-colour"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TIP_COLOR_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// Upload a coloured stamp's colour plane.
///
/// The premultiply is `umber-core`'s — [`TipMask::colour_premultiplied`] — for
/// the reason every other conversion in this codebase lives there: it is
/// arithmetic with an exact inverse and it is testable without a device. This
/// function is the `write_texture` and nothing else.
fn upload_tip_color(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    mask: &TipMask,
    rgba: &[u8],
) -> wgpu::Texture {
    let texture = make_tip_color_texture(device, mask.width(), mask.height());
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(mask.width() * 4),
            rows_per_image: Some(mask.height()),
        },
        wgpu::Extent3d {
            width: mask.width(),
            height: mask.height(),
            depth_or_array_layers: 1,
        },
    );
    texture
}

/// Single-channel coverage storage — a brush tip, a paper tile or a selection
/// mask. One function because they are the same texture with different
/// contents, and a second copy of this descriptor is a second place for the
/// format to drift from the stroke scratch these all feed.
fn make_coverage_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    label: &str,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: STROKE_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

#[allow(clippy::too_many_arguments)]
fn make_dab_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniforms: &wgpu::Buffer,
    tip: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    grain: &wgpu::TextureView,
    grain_sampler: &wgpu::Sampler,
    selection: &wgpu::TextureView,
    tip_color: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("dab-bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(tip),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(grain),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::Sampler(grain_sampler),
            },
            wgpu::BindGroupEntry {
                binding: 5,
                resource: wgpu::BindingResource::TextureView(selection),
            },
            wgpu::BindGroupEntry {
                binding: 6,
                resource: wgpu::BindingResource::TextureView(tip_color),
            },
        ],
    })
}

/// The stroke's coverage scratch: one channel, canvas-sized.
fn make_stroke_texture(device: &wgpu::Device, size: UVec2) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("umber-stroke-scratch"),
        size: wgpu::Extent3d {
            width: size.x.max(1),
            height: size.y.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: STROKE_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    })
}

fn make_stroke_color_texture(device: &wgpu::Device, size: UVec2) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("umber-stroke-colour"),
        size: wgpu::Extent3d {
            width: size.x.max(1),
            height: size.y.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: STROKE_COLOR_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    })
}

fn make_composite_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniforms: &wgpu::Buffer,
    layers: &wgpu::TextureView,
    stroke: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    stroke_color: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("composite-bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(layers),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(stroke),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: wgpu::BindingResource::TextureView(stroke_color),
            },
        ],
    })
}

fn make_commit_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniforms: &wgpu::Buffer,
    stroke: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
    stroke_color: &wgpu::TextureView,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("commit-bg"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(stroke),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(stroke_color),
            },
        ],
    })
}

impl EffectCache {
    /// Give up every slice and the working set with them.
    ///
    /// What a document with no effects does, and it is the whole of §4.2 in one
    /// method: an effect slice goes straight back on the free list, with no
    /// parking and no undo-budget arithmetic, because no `PixelPatch` can ever
    /// name one. The working set goes too — 400 MB at 10000² is not something to
    /// hold in case somebody switches a shadow back on.
    fn forget_all(&mut self) {
        self.forget_entries();
        self.scratch = None;
        self.dropped = 0;
    }

    /// Give up every slice but keep the working set, for a bake that is about to
    /// re-run from a different `base`.
    fn forget_entries(&mut self) {
        self.entries.clear();
        self.free.clear();
        self.next = 0;
    }

    /// Release every entry whose key nothing wants any more.
    fn retain_only(&mut self, keys: &[(u32, Option<u32>, EffectKind)]) {
        let base = self.base;
        let free = &mut self.free;
        self.entries.retain(|e| {
            if keys.contains(&(e.source, e.mask, e.kind)) {
                return true;
            }
            free.push(e.slot - base);
            false
        });
        self.free.sort_unstable();
    }

    /// The slice this key holds, allocating one if it does not hold any.
    fn slot_for(&mut self, key: (u32, Option<u32>, EffectKind), capacity: usize) -> Option<u32> {
        if let Some(e) = self
            .entries
            .iter()
            .find(|e| (e.source, e.mask, e.kind) == key)
        {
            return Some(e.slot);
        }
        let offset = match self.free.pop() {
            Some(o) => o,
            None if (self.next as usize) < capacity => {
                self.next += 1;
                self.next - 1
            }
            None => return None,
        };
        let slot = self.base + offset;
        self.entries.push(CachedEffect {
            source: key.0,
            mask: key.1,
            kind: key.2,
            slot,
            // Never baked. `slot_revision` counts from zero and only rises, so
            // this can never be mistaken for a real reading.
            source_revision: u64::MAX,
            mask_revision: u64::MAX,
            params: u64::MAX,
            live: false,
        });
        Some(slot)
    }

    /// Is the slice this stamp names already the picture the stamp describes?
    ///
    /// Everything the *pixels* depend on and nothing else: the two slice
    /// revisions, the parameter hash, and whether a live stroke was folded in.
    /// A stroke in flight makes the stamp differ every frame through the
    /// revisions only after it commits, so the `live` flag is what carries the
    /// frames in between — and, more importantly, what makes the *end* of a
    /// cancelled stroke stale, since a cancel writes no pixels at all.
    fn is_fresh(&self, stamp: &CachedEffect) -> bool {
        self.entries.iter().any(|e| {
            (e.source, e.mask, e.kind) == (stamp.source, stamp.mask, stamp.kind)
                && e.slot == stamp.slot
                && e.source_revision == stamp.source_revision
                && e.mask_revision == stamp.mask_revision
                && e.params == stamp.params
                && e.live == stamp.live
                // A live stroke is never fresh: the scratch has moved under it
                // and nothing counts how far. That is what makes the shadow
                // follow the brush.
                && !e.live
        })
    }

    fn record(&mut self, stamp: CachedEffect) {
        let key = (stamp.source, stamp.mask, stamp.kind);
        if let Some(e) = self
            .entries
            .iter_mut()
            .find(|e| (e.source, e.mask, e.kind) == key)
        {
            *e = stamp;
        }
    }
}

impl EffectScratch {
    fn new(
        device: &wgpu::Device,
        shared: &Shared,
        size: UVec2,
        layers: &LayerStore,
        stroke_view: &wgpu::TextureView,
        seeds: bool,
        band: bool,
    ) -> Self {
        let mut textures = Vec::new();
        let mut plane = |label: &str| {
            let t = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: size.x,
                    height: size.y,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                // `R8Unorm`: one channel, because coverage is all any of these
                // hold, and **no transfer function**, which is what §3.2 requires
                // of a separable blur's intermediate. It is also exactly as wide
                // as the alpha channel the effect ends up in, so it adds no loss
                // of its own — the same argument the stroke scratch makes.
                format: STROKE_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let view = t.create_view(&wgpu::TextureViewDescriptor::default());
            textures.push(t);
            view
        };
        let coverage = plane("umber-effect-coverage");
        let grown = plane("umber-effect-grown");
        let blur = [plane("umber-effect-blur-0"), plane("umber-effect-blur-1")];
        // Before the placeholders below, because both closures hold `textures`
        // and only one may at a time.
        let band = band.then(|| plane("umber-effect-band"));
        // Bound wherever a pass does not read a coverage field, so that a
        // texture this pass is *writing* is never also named by its bind group.
        // A texture rather than nothing, because the layout is one layout for all
        // seven passes and every binding in it has to be filled.
        let blank = {
            let t = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("umber-effect-blank"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: STROKE_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = t.create_view(&wgpu::TextureViewDescriptor::default());
            textures.push(t);
            view
        };
        // And the same for the layer array, which the *resolve* writes a slice
        // of — so every pass but the extract binds this instead.
        let blank_array = {
            let t = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("umber-effect-blank-array"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: LAYER_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = t.create_view(&wgpu::TextureViewDescriptor {
                label: Some("umber-effect-blank-array"),
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                ..Default::default()
            });
            textures.push(t);
            view
        };

        let mut seed_plane = |label: &str, w: u32, h: u32| {
            let t = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: SEED_FORMAT,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let view = t.create_view(&wgpu::TextureViewDescriptor::default());
            textures.push(t);
            view
        };
        // A 1x1 stand-in bound wherever a pass does not read the seeds. Needed
        // rather than tidy: `fs_grow` names that binding even on the path that
        // returns before reading it, so the layout demands one — the same reason
        // the tip and the paper have placeholders.
        let seed_placeholder = seed_plane("umber-effect-seed-none", 1, 1);
        let seeds = seeds.then(|| {
            [
                seed_plane("umber-effect-seed-0", size.x, size.y),
                seed_plane("umber-effect-seed-1", size.x, size.y),
            ]
        });

        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("effect-uniforms"),
            size: EFFECT_PASS_BLOCKS * EFFECT_BLOCK_STRIDE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // One bind group per (src, seeds) pairing a pass can want, built here and
        // reused by every effect for the document's life. The views they name are
        // fixed; only the uniform block varies per pass, and that is a dynamic
        // offset.
        let bind = |label: &str,
                    array: &wgpu::TextureView,
                    src: &wgpu::TextureView,
                    cov: &wgpu::TextureView,
                    seed: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &shared.effect_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &uniforms,
                            offset: 0,
                            size: Some(
                                std::num::NonZeroU64::new(
                                    std::mem::size_of::<EffectUniforms>() as u64
                                )
                                .expect("the uniform block is not empty"),
                            ),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(array),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(src),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(cov),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(seed),
                    },
                ],
            })
        };
        let none = &seed_placeholder;
        let seed0 = seeds.as_ref().map_or(none, |s| &s[0]);
        let seed1 = seeds.as_ref().map_or(none, |s| &s[1]);
        let a = &blank_array;
        let b = &blank;
        // In [`EffectBind`]'s order, and the order is the enum's discriminants —
        // `debug_assert` below is the only thing holding the two together, which
        // is why they are written adjacently.
        let mut binds = Vec::with_capacity(EFFECT_BIND_COUNT);
        binds.push(bind(
            "effect-extract",
            &layers.array_view,
            stroke_view,
            b,
            none,
        ));
        binds.push(bind("effect-coverage", a, b, &coverage, none));
        binds.push(bind("effect-grow-0", a, b, &coverage, seed0));
        binds.push(bind("effect-grow-1", a, b, &coverage, seed1));
        binds.push(bind("effect-flood-0", a, b, b, seed0));
        binds.push(bind("effect-flood-1", a, b, b, seed1));
        binds.push(bind("effect-src-grown", a, &grown, b, none));
        binds.push(bind("effect-src-blur-0", a, &blur[0], b, none));
        binds.push(bind("effect-src-blur-1", a, &blur[1], b, none));
        binds.push(bind("effect-resolve-grown", a, &grown, &coverage, none));
        binds.push(bind("effect-resolve-blur-0", a, &blur[0], &coverage, none));
        binds.push(bind("effect-resolve-blur-1", a, &blur[1], &coverage, none));
        binds.push(bind("effect-src-coverage", a, &coverage, b, none));
        binds.push(bind(
            "effect-resolve-coverage",
            a,
            &coverage,
            &coverage,
            none,
        ));
        let band_src = band.as_ref().unwrap_or(b);
        binds.push(bind("effect-combine-0", a, band_src, &coverage, seed0));
        binds.push(bind("effect-combine-1", a, band_src, &coverage, seed1));
        debug_assert_eq!(binds.len(), EFFECT_BIND_COUNT);

        Self {
            size,
            coverage,
            grown,
            blur,
            seeds,
            band,
            textures,
            uniforms,
            binds,
            bound_capacity: layers.capacity,
        }
    }
}

/// One effect's draw entry.
///
/// Every field is a decision and three of them are worth naming:
///
/// * **The opacity is the effect's times the layer's.** §4 says an effect carries
///   "its own opacity", and it does; multiplying the layer's in is the answer
///   every application gives and the only one that is not absurd — a layer faded
///   to nothing whose drop shadow stayed at full strength would be a shadow of a
///   picture that is not there.
/// * **No mask.** The coverage the effect was baked from was already multiplied
///   by it, so applying it again at composite time would apply it twice — the
///   same double application the lift's `min` and the clipboard's exist to
///   refuse.
/// * **`clipped` is the inner effect's whole confinement**, and for an outer one
///   it is the layer's own flag so that a clipped layer's effects are clipped
///   with it (§9.3). Note what that costs: an *inner* effect on a *clipped* layer
///   is bounded by the clip group's base rather than by its own layer, because
///   one flag cannot say both. §9.3 accepts it; it is the one place the
///   asymmetry is not free.
fn effect_draw(effect: &Effect, slot: u32, entry: &LayerEffects<'_>) -> LayerDraw {
    LayerDraw {
        slot,
        opacity: (effect.opacity * entry.draw.opacity).clamp(0.0, 1.0),
        blend: effect.blend.index(),
        visible: entry.draw.visible,
        mask: None,
        clipped: effect.is_inner() || entry.draw.clipped,
    }
}

/// What shape one effect is, how far its field has to reach, and which way its
/// flood is seeded.
///
/// **One statement of it**, because three places ask: the planner, the live-bake
/// gate and the working set's allocation. Reading it three times is three chances
/// to disagree about whether an effect needs a flood at all — and the answer
/// decides both the cost and, for a centred outline, whether the band plane has
/// to exist.
struct EffectField {
    shape: EffectShape,
    /// How far the band or the dilate reaches, in document pixels. Zero means no
    /// distance field is needed at all.
    reach: f32,
    /// Seed the flood on the complement, which turns the field into an *inward*
    /// distance. An inside outline's whole mechanism.
    ///
    /// **For [`EffectShape::Centre`] this is the *second* flood's**, and the
    /// planner overrides it to `0` for the first: that one has to be the outward
    /// field, because the combining pass reads the outward band out of the band
    /// plane and computes the inward one itself. Reading this field as "which way
    /// the flood goes" is right for every other shape and is half the story for
    /// that one.
    invert: bool,
}

fn effect_field(effect: &Effect) -> EffectField {
    match (effect.kind, effect.position) {
        // The full width, straddling the edge: half of it each side. This is the
        // one position that needs two fields — see `EffectShape::Centre` — and
        // the reach is the half width, which is what *each* band is, so the two
        // together span `spread`. `invert` is the second flood's; the planner
        // runs the first with it clear. See the field.
        (EffectKind::Outline, OutlinePosition::Centre) => EffectField {
            shape: EffectShape::Centre,
            reach: effect.spread * 0.5,
            invert: true,
        },
        (EffectKind::Outline, OutlinePosition::Inside) => EffectField {
            shape: EffectShape::Inner,
            reach: effect.spread,
            invert: true,
        },
        (EffectKind::Outline, OutlinePosition::Outside) => EffectField {
            shape: EffectShape::Outer,
            reach: effect.spread,
            invert: false,
        },
        (EffectKind::DropShadow, _) => EffectField {
            shape: EffectShape::Dilate,
            reach: effect.spread,
            invert: false,
        },
    }
}

/// [`CanvasRenderer::effect_bakes_live`]'s rule, as a function of the canvas's
/// area rather than of a renderer.
///
/// A free function so the rule is testable without a device, which is the
/// division `band_rows` and `grown_capacity` already keep — and it is worth it
/// here because the rule is the *reason* the gate stopped being canvas-wide, so
/// "this effect is cheap and that one is not" should be a test rather than a
/// sentence.
///
/// It reads [`effect_field`] and [`tent_for`], the same two functions
/// `plan_effect` reads, so "judged cheap" and "no flood planned" cannot diverge:
/// `plan_effect`'s own `grow` is `effect_field(effect).reach > 0.0`, which is the
/// exact negation of the first clause here.
fn effect_bakes_live_at(pixels: u64, effect: &Effect) -> bool {
    pixels <= EFFECT_LIVE_PIXELS || effect_field(effect).reach <= 0.0
}

/// The tent the blur will actually run, as `(downsample, box radius)`.
///
/// A radius of zero means **no blur pass at all**, which is the exact identity
/// the selection's feather and the brush's grain both keep.
///
/// Two discrete box passes of `2r + 1` taps at a reduction of `d` have a
/// continuous half support of `d(2r + 1)`, so the radius that draws a soft edge
/// of `softness` document pixels is `(softness/d - 1)/2`. The reduction is
/// [`EFFECT_DOWN`] once the radius is wide enough for that to round to something
/// useful and 1 below it — see [`EFFECT_FULL_RES_SOFTNESS`], which is where the
/// argument for having two lives.
fn tent_for(softness: f32) -> (u32, i32) {
    if softness <= 0.0 {
        return (1, 0);
    }
    let down = if softness >= EFFECT_FULL_RES_SOFTNESS {
        EFFECT_DOWN
    } else {
        1
    };
    let radius = ((softness / down as f32 - 1.0) / 2.0).round().max(1.0) as i32;
    (down, radius)
}

/// Everything about an effect that the **pixels** depend on, as one number.
///
/// Deliberately not the whole struct: `opacity` and `blend` belong to the draw
/// and are applied by `composite.wgsl`, so dragging either slider must cost no
/// rebake. `enabled` is absent because a disabled effect has no entry at all.
///
/// `to_bits` rather than a comparison, because this is a cache key and two
/// values that differ in the last bit really do produce different pixels — and
/// because `f32` is not `Hash`. NaN hashes to itself, which is right: a
/// parameter that has gone NaN should not read as unchanged.
fn effect_params_hash(effect: &Effect) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    effect.kind.hash(&mut h);
    effect.position.hash(&mut h);
    for v in [
        effect.spread,
        effect.softness,
        effect.angle,
        effect.distance,
        effect.color.r,
        effect.color.g,
        effect.color.b,
        effect.color.a,
    ] {
        v.to_bits().hash(&mut h);
    }
    h.finish()
}

fn uniform_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn texture_array_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2Array,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `MAX_DRAWS` the shader compiles, as an integer.
    ///
    /// Parsed out of the WGSL text because that is the only way to read it: the
    /// shader is a string until naga sees it, so nothing in Rust can name the
    /// constant. Deliberately strict about the shape of the line — a parse that
    /// quietly failed and answered a default would be a guard that agrees with
    /// whatever it is compared against.
    fn shader_max_draws() -> usize {
        const NEEDLE: &str = "const MAX_DRAWS: u32 = ";
        let at = COMPOSITE_WGSL
            .find(NEEDLE)
            .expect("composite.wgsl no longer declares `const MAX_DRAWS: u32 = ...`");
        let rest = &COMPOSITE_WGSL[at + NEEDLE.len()..];
        let end = rest
            .find("u;")
            .expect("`MAX_DRAWS` is no longer a `u32` literal ending in `u;`");
        rest[..end]
            .trim()
            .parse()
            .expect("`MAX_DRAWS` is not a plain decimal literal")
    }

    /// The shader text alone. **Not** [`COMPOSITE_SHADER`], which has
    /// `blend.wgsl` concatenated in front of it — the needles below want the
    /// one file the constant and the arrays are declared in.
    const COMPOSITE_WGSL: &str = include_str!("../shaders/composite.wgsl");
    const EFFECT_WGSL: &str = include_str!("../shaders/effect.wgsl");

    /// **Every shape the planner can ask for is a shape the shader names**, and
    /// with the same number.
    ///
    /// `EffectShape`'s discriminants cross into WGSL as a plain `u32` in a
    /// uniform, so the two sets of five are one format with no compiler between
    /// them. `fs_grow`'s `switch` needs a `default` and that arm used to be
    /// `SHAPE_DILATE`, so a mode it had never heard of drew the union of the shape
    /// and the band — a filled silhouette in the effect's colour instead of a
    /// ring, which is the loudest wrong picture arriving as the quietest failure.
    /// It now draws nothing, and this is what turns "a shape was added on one side
    /// only" into a red test rather than a silhouette.
    ///
    /// The arms are an exhaustive `match`, so a sixth variant fails the **build**
    /// here rather than being quietly absent from the list — the rule an `ALL`
    /// array is guarded by, applied to a set that has no `ALL`.
    #[test]
    fn the_shader_knows_every_shape_the_planner_can_ask_for() {
        let named = |name: &str| -> Option<u32> {
            let needle = format!("const {name}: u32 = ");
            let at = EFFECT_WGSL.find(&needle)? + needle.len();
            let rest = &EFFECT_WGSL[at..];
            let end = rest.find('u')?;
            rest[..end].trim().parse().ok()
        };
        let spelling = |shape: EffectShape| match shape {
            EffectShape::Dilate => "SHAPE_DILATE",
            EffectShape::Outer => "SHAPE_OUTER",
            EffectShape::Inner => "SHAPE_INNER",
            EffectShape::Centre => "SHAPE_CENTRE",
            EffectShape::Raw => "SHAPE_RAW",
        };
        let all = [
            EffectShape::Dilate,
            EffectShape::Outer,
            EffectShape::Inner,
            EffectShape::Centre,
            EffectShape::Raw,
        ];
        for shape in all {
            let name = spelling(shape);
            assert_eq!(
                named(name),
                Some(shape as u32),
                "{name} in effect.wgsl does not match {shape:?} on this side"
            );
            // And the `switch` really has an arm for it, rather than leaning on a
            // `default` that now draws nothing. Any `case` line naming it will do:
            // WGSL allows `case A, B:` and two shapes genuinely do share one arm,
            // so requiring the name to start the arm would fail on the shader as
            // written.
            assert!(
                EFFECT_WGSL
                    .lines()
                    .any(|l| l.trim_start().starts_with("case ") && l.contains(name)),
                "fs_grow has no `case` naming {name}, so it would draw nothing"
            );
        }
        // Five distinct numbers, or two shapes share an arm.
        let mut numbers: Vec<u32> = all.iter().map(|s| *s as u32).collect();
        numbers.sort_unstable();
        numbers.dedup();
        assert_eq!(numbers.len(), all.len());
    }

    /// Every length the shader declares a `vec4<f32>` array at.
    ///
    /// **Reading the constant is not enough**, and this is the gap the first
    /// draft had. `shader_max_draws` proves what `MAX_DRAWS` *is*, not that the
    /// arrays use it: leave the constant at 191 and write
    /// `array<vec4<f32>, 64>` and the WGSL struct is 2160 bytes against a
    /// 6224-byte buffer. That direction **passes validation** — a binding may
    /// be larger than the struct — and the composite then reads `extra` as
    /// `layers` for every draw past the 63rd. The reverse fails loudly, which
    /// is why only this one needed catching.
    fn shader_array_lengths() -> Vec<&'static str> {
        const OPEN: &str = "array<vec4<f32>, ";
        COMPOSITE_WGSL
            .match_indices(OPEN)
            .map(|(at, _)| {
                let rest = &COMPOSITE_WGSL[at + OPEN.len()..];
                let end = rest.find('>').expect("an unterminated array type");
                rest[..end].trim()
            })
            .collect()
    }

    /// **Three numbers have to agree, and this is what says so.**
    ///
    /// `LayerStack::MAX` bounds stack entries; [`MAX_DRAWS`] here and
    /// `MAX_DRAWS` in `composite.wgsl` size the composite pass's two uniform
    /// arrays, and must be the same or the Rust struct and the WGSL one stop
    /// matching byte for byte. There used to be two, all equal; a layer's
    /// effects each composite as a draw of their own, so the stack cap and the
    /// draw cap are now different quantities. A later change to any one of the
    /// three is exactly the kind of thing that breaks in silence.
    #[test]
    fn the_three_draw_capacities_agree() {
        assert_eq!(
            MAX_LAYERS,
            umber_core::LayerStack::MAX,
            "the stack cap here and in umber-core have drifted"
        );
        assert_eq!(
            MAX_DRAWS,
            shader_max_draws(),
            "MAX_DRAWS in canvas.rs and composite.wgsl have drifted"
        );

        // And that the arrays are declared at it, not merely that the constant
        // holds it. See `shader_array_lengths` for the failure this catches.
        let lengths = shader_array_lengths();
        assert_eq!(
            lengths.len(),
            2,
            "composite.wgsl no longer declares exactly two vec4 arrays: {lengths:?}"
        );
        for len in lengths {
            assert_eq!(
                len, "MAX_DRAWS",
                "a uniform array is sized by something other than MAX_DRAWS"
            );
        }
    }

    /// The one number here that is not ours, and the two that come out of it.
    ///
    /// **The derivation is what is tested, at inputs other than the shipped
    /// ones, and it is tested by *calling* it.** Asserting `MAX_SLOTS ==
    /// MAX_LAYERS * 2 + 1 + (MAX_DRAWS - MAX_LAYERS)` is a copy of the formula
    /// and cannot fail when the formula is wrong. Nor is it enough to recompute
    /// the subtraction in the test and check it against itself: `a - b + b ==
    /// a` holds whatever [`effect_slices`] does. Both drafts of this test made
    /// one of those two mistakes. What runs below is the real
    /// [`effect_slices`] and [`draws`], and the claims are about *them*.
    #[test]
    fn the_slice_ceiling_agrees_with_umber_core() {
        assert_eq!(MAX_SLOTS as u32, umber_core::LayerStack::MAX_SLOTS);
        assert_eq!(MAX_LAYERS, umber_core::LayerStack::MAX);

        // The shipped constants really are what the derivation answers, so
        // everything proved about the functions is proved about them.
        assert_eq!(MAX_EFFECT_SLICES, effect_slices(MAX_LAYERS, MAX_SLOTS));
        assert_eq!(MAX_DRAWS - MAX_LAYERS, MAX_EFFECT_SLICES);
        assert_eq!(MAX_EFFECT_SLICES, 127);
        assert_eq!(MAX_DRAWS, 191);

        // The model's budget and this array's capacity are the same quantity
        // spelled in two crates, and **this is the only place both can be
        // seen**: `umber-core` may not depend on wgpu, so it cannot derive its
        // figure from the device's ceiling and carries a literal instead. An
        // effect draw reads an effect slice, one for one, so a model that let
        // more effects be added than there are slices would be promising a
        // draw with nothing to read.
        assert_eq!(
            umber_core::effect::MAX_ENABLED,
            MAX_EFFECT_SLICES,
            "the model's effect budget and the renderer's slice budget are one number",
        );

        // The ceiling is fixed by the device, so it is the one input that does
        // not vary. `MAX_LAYERS` is ours and may move, which is the whole
        // reason these are functions.
        for layers in [1usize, 8, 32, 64, 100, 127] {
            let effects = effect_slices(layers, MAX_SLOTS);
            // One draw per layer and one per effect slice, which is how
            // `MAX_DRAWS` is built.
            let total = layers + effects;

            // Every slice is spoken for exactly once: a layer, a mask, the
            // float's spare, or an effect. Drop the `+ 1` from
            // `effect_slices` and this is the assertion that goes red — and so
            // does a body that is right at 64 and wrong at 1, which is the
            // mutation this loop exists for.
            assert_eq!(layers * 2 + 1 + effects, MAX_SLOTS, "{layers} layers");
            // The draw list can never outrun the array it draws from, which is
            // the promise 192 broke.
            assert!(total <= MAX_SLOTS, "{layers} layers: {total} draws");
        }

        // Raising it allocates nothing: the array starts at `INITIAL_SLOTS` and
        // `ensure_slots` grows towards what is actually claimed. That it does
        // not *overshoot* towards the ceiling is `grown_capacity`'s doing and
        // is guarded separately — see
        // `a_document_does_not_double_its_layer_array_to_reach_one_more_slice`,
        // which exists because raising this constant is what broke it.
        assert!(INITIAL_SLOTS < MAX_SLOTS as u32);
    }

    /// The composite uniform is the one block here large enough for the
    /// question to be worth asking, and raising [`MAX_DRAWS`] is what makes it
    /// so. Both halves are checked: the size the arithmetic in
    /// [`ViewUniforms`]' doc comment claims, and that it clears the smallest
    /// binding a device Umber will run on has to offer.
    #[test]
    fn the_view_uniform_fits_the_smallest_binding_a_device_must_offer() {
        // The head is **measured**, not restated. `112 % 16 == 0` written as a
        // literal is a tautology that cannot fail; what has to be true is that
        // the offset Rust actually gives `layers` is 16-aligned, because WGSL
        // aligns an `array<vec4<f32>>` to 16 and would insert padding there
        // that `#[repr(C)]` does not — leaving the buffer short by however
        // much, and every draw after the gap reading the wrong entry.
        let head = std::mem::offset_of!(ViewUniforms, layers);
        assert_eq!(head, 112, "the scalar head of the block changed size");
        assert_eq!(head % 16, 0, "WGSL would pad where Rust does not");
        assert_eq!(
            std::mem::offset_of!(ViewUniforms, extra),
            head + MAX_DRAWS * 16,
            "the two arrays are not back to back"
        );

        let size = std::mem::size_of::<ViewUniforms>();
        assert_eq!(size, head + MAX_DRAWS * 32);
        assert_eq!(size, 6224, "the figure in the doc comment is stale");
        // The struct's own alignment in WGSL is 16, so its size rounds up to a
        // multiple of it. Rust's is 4, and a mismatch here would be tail
        // padding on one side only.
        assert_eq!(size % 16, 0);

        // `Gpu::new` asks for `downlevel_defaults`, and `using_resolution`
        // raises only the texture dimensions, so this is the limit in force on
        // every adapter Umber accepts.
        let limit = wgpu::Limits::downlevel_defaults().max_uniform_buffer_binding_size as usize;
        assert_eq!(limit, 16 << 10, "downlevel_defaults moved under us");
        assert!(
            size <= limit,
            "the composite uniform is {size} bytes against a {limit}-byte binding limit"
        );
    }

    /// One slice of a square canvas, in bytes.
    fn slice_of(side: u64) -> u64 {
        side * side * LAYER_BYTES_PER_PIXEL
    }

    /// Canvases to sweep the growth rule over.
    ///
    /// **Squares alone are not a sweep of canvas sizes, and powers of two alone
    /// are not a sweep of slice sizes.** Both were true of the first draft and
    /// each hid something. `slice_bytes` is `x * y * 4` and nothing makes a
    /// canvas square, so `1024x512` — an ordinary shape — reaches a waste no
    /// square canvas does. And every power-of-two side divides the budget
    /// *exactly*, so `budget / slice` and `budget.div_ceil(slice)` agree at
    /// every one of them: two rounding mutations in [`growth_quantum`] passed
    /// the entire suite until `1920x1080` and `1500x1500` were added, where the
    /// quantum is 32 against 33 and 29 against 30.
    ///
    /// So the list is deliberately a mix: powers of two, a non-square, and four
    /// real display and print sizes whose slice divides nothing neatly.
    const CANVASES: [(u64, u64); 14] = [
        (1, 1),
        (64, 64),
        (256, 256),
        (512, 512),
        (1024, 512),
        (1024, 1024),
        (1500, 1500),
        (1920, 1080),
        (2048, 2048),
        (2560, 1440),
        (3000, 2000),
        (4096, 4096),
        (8192, 8192),
        (10_000, 10_000),
    ];

    /// **The regression this policy exists for, stated as the allocation it
    /// makes.**
    ///
    /// 64 layers each with a mask is 128 slices and a legal document with no
    /// effects in it; `begin_float` then asks for the 129th. Under plain
    /// doubling that allocates 256 — 4.29 GB at 2048² against the 2.16 GB
    /// asked for, with the old array still alive during the copy and no
    /// shrinking afterwards.
    ///
    /// It passed for the wrong reason before `MAX_SLOTS` moved: `.min(129)`
    /// clamped the overshoot away, so the doubling was never exercised and the
    /// clamp was load-bearing without saying so. **Nothing measured the
    /// allocated capacity at all** — the only two assertions in the suite are
    /// `< 8` and `>= 8` — which is why raising the ceiling to 256 changed the
    /// behaviour in silence.
    /// **The rule now has two neighbours and has to be told apart from both**,
    /// which is why the figures below are exact rather than bounds: plain
    /// doubling gives 256 where this gives 144, and growing to `needed` exactly
    /// gives 129. An assertion that merely said "less than 256" would pass
    /// under exact growth, and one that said "129" would be pinning the
    /// accident this function exists to explain.
    #[test]
    fn a_document_does_not_double_its_layer_array_to_reach_one_more_slice() {
        // 2048², the canvas the arithmetic was worked out on. One quantum of
        // 16 past 129. Plain doubling: 256. Exact: 129.
        // The waste that buys, measured from what the function returns rather
        // than from the literal beside it. Written `144 - 129` at first, which
        // is two literals and an assertion no change to the rule could fail —
        // exactly the `a - b + b == a` shape this file warns about two hundred
        // lines up, in a line whose comment claimed it could not drift.
        let cap = grown_capacity(128, 129, slice_of(2048));
        assert_eq!(cap, 144);
        let waste = u64::from(cap - 129) * slice_of(2048);
        assert_eq!(waste, 251_658_240);
        assert!(waste <= GROWTH_DOUBLING_BUDGET_BYTES);

        // A case that separates all three at once, because the one above does
        // not separate the quantum from plain doubling on every canvas.
        // Plain doubling: 128. Exact: 100. Quantum: 112.
        assert_eq!(grown_capacity(99, 100, slice_of(2048)), 112);

        // At 10000² the quantum is one slice, so the rule *is* exact growth —
        // which is the point of deriving it from the budget rather than
        // choosing it. Plain doubling would still give 256 here, 50 GB of
        // overshoot.
        assert_eq!(growth_quantum(slice_of(10_000)), 1);
        assert_eq!(grown_capacity(128, 129, slice_of(10_000)), 129);

        // The clamp in `ensure_slots` is not what is doing any of this: every
        // answer above is well under the 256 ceiling, so a reverted policy
        // would return 256 and the clamp would pass it straight through.
        assert!(144 < MAX_SLOTS as u32);
    }

    /// The array a renderer is *born* with answers to the same budget.
    ///
    /// It is allocated before `ensure_slots` can be asked anything, so a
    /// literal four slices is 1.6 GB at 10000² — 1.14 GB of it speculation for
    /// a document with one layer, nine times the bound
    /// [`GROWTH_DOUBLING_BUDGET_BYTES`] states twenty lines above it. Two
    /// constants disagreeing about the same question.
    #[test]
    fn the_array_a_renderer_starts_with_answers_to_the_budget_too() {
        // Cheap canvases are untouched: four is four.
        assert_eq!(initial_slots(slice_of(256)), INITIAL_SLOTS);
        assert_eq!(initial_slots(slice_of(2048)), INITIAL_SLOTS);
        // 4096²: 64 MiB a slice, so four is 256 MiB — exactly the budget.
        assert_eq!(initial_slots(slice_of(4096)), 4);
        // 10000²: 400 MB a slice, so one is already over and one is what it
        // takes. Never zero — a renderer with no slices has nowhere to paint.
        assert_eq!(initial_slots(slice_of(10_000)), 1);
        // 8192²: 256 MiB a slice, one fits exactly.
        assert_eq!(initial_slots(slice_of(8192)), 1);
        // Total, including the degenerate canvas `slice_bytes` can report.
        for side in [1u64, 64, 512, 2048, 4096, 10_000, 40_000] {
            let n = initial_slots(slice_of(side));
            assert!((1..=INITIAL_SLOTS).contains(&n), "{side}² asked for {n}");
        }
        assert_eq!(initial_slots(0), INITIAL_SLOTS);
    }

    /// Growth stays amortised while a slice is cheap, which is the whole reason
    /// doubling is there.
    #[test]
    fn a_small_canvas_still_doubles() {
        // 256², 256 KiB a slice: a handful of slices is nothing, so a document
        // adding its fifth layer gets room for eight.
        assert_eq!(grown_capacity(4, 5, slice_of(256)), 8);
        // And from one, up to the first power of two that holds it.
        assert_eq!(grown_capacity(1, 5, slice_of(256)), 8);
    }

    /// The budget is in bytes, so the same slice count behaves differently on
    /// different canvases — which is the point of stating it that way.
    #[test]
    fn the_growth_budget_is_measured_in_bytes_not_slices() {
        // 16 MiB a slice: doubling to 16 slices is 256 MiB and allowed, to 32
        // is 512 MiB and refused — past which the quantum of 16 takes over,
        // which happens to agree with doubling at 17 and not at 100.
        assert_eq!(grown_capacity(8, 9, slice_of(2048)), 16);
        assert_eq!(grown_capacity(16, 17, slice_of(2048)), 32);
        assert_eq!(grown_capacity(99, 100, slice_of(2048)), 112);
        // The quantum falls as the canvas grows, because it is the budget
        // divided by a slice. Never to zero — `div_ceil` by zero panics.
        assert_eq!(growth_quantum(slice_of(1024)), 64);
        assert_eq!(growth_quantum(slice_of(2048)), 16);
        assert_eq!(growth_quantum(slice_of(4096)), 4);
        assert_eq!(growth_quantum(slice_of(8192)), 1);

        // **Canvases where the budget is not a whole number of slices**, which
        // every power-of-two square hides: these are the only assertions that
        // can tell the floor from a ceil or from a rounding to a power of two,
        // and without them both mutations pass the suite. 1920×1080 divides the
        // budget 32.36 times and 1500² 29.8.
        assert_eq!(growth_quantum(1920 * 1080 * LAYER_BYTES_PER_PIXEL), 32);
        assert_eq!(growth_quantum(1500 * 1500 * LAYER_BYTES_PER_PIXEL), 29);
        assert_eq!(growth_quantum(2560 * 1440 * LAYER_BYTES_PER_PIXEL), 18);
        assert_eq!(growth_quantum(3000 * 2000 * LAYER_BYTES_PER_PIXEL), 11);
        // And what that means for a real allocation, so the quantum is pinned
        // where it is *used* and not only where it is computed.
        assert_eq!(
            grown_capacity(33, 34, 1920 * 1080 * LAYER_BYTES_PER_PIXEL),
            64
        );
        assert_eq!(
            grown_capacity(16, 17, 1500 * 1500 * LAYER_BYTES_PER_PIXEL),
            29
        );

        for (w, h) in CANVASES {
            assert!(
                growth_quantum(w * h * LAYER_BYTES_PER_PIXEL) >= 1,
                "{w}x{h}"
            );
        }
        assert!(growth_quantum(0) >= 1);
        // 400 MB a slice: one slice is already over the budget, so the quantum
        // is one, every growth is exact, and nothing is speculated.
        for needed in [2u32, 3, 9, 100] {
            assert_eq!(
                grown_capacity(needed - 1, needed, slice_of(10_000)),
                needed,
                "{needed} slices at 10000²"
            );
        }
    }

    /// **The overshoot is bounded by the budget, on every canvas size and every
    /// slice count**, which is the property the whole rule exists for and is
    /// stronger than the cases above.
    ///
    /// Both halves of the rule are bounded and by different arguments, which is
    /// why the sweep is worth more than either. Doubling runs only while the
    /// *resulting* array is inside the budget, so `capacity × slice ≤ budget`,
    /// and stops the first time `capacity >= needed`, so `capacity < 2 × needed`
    /// and the waste is under `capacity / 2` — half a budget. Rounding up is at
    /// most one slice short of a whole quantum, and a quantum is by construction
    /// the most slices that fit in the budget. So one budget covers both.
    ///
    /// Swept rather than argued, because a proof about the code is not a
    /// statement about the code. Measured, the worst is **254 MiB at 1024x512**
    /// asking for a 129th slice — the quantum branch, one slice short of a full
    /// quantum of 128. It falls to zero at 10000², where the quantum is a single
    /// slice and nothing is ever speculated.
    ///
    /// **That worst case is on a non-square canvas, which the first draft of
    /// this sweep could not see**: it walked square sides only, while
    /// `slice_bytes` is `x * y * 4` and nothing constrains a canvas to be
    /// square. The bound held either way — see [`CANVASES`] for what else the
    /// narrow list hid, which did not.
    ///
    /// **`current` is swept and the first draft did not sweep it**, which made
    /// this exactly the guard CLAUDE.md warns about: one whose comment claims
    /// more reach than its mutations demonstrate. Starting every case from a
    /// cold zero, `let mut capacity = current.max(1).next_power_of_two();`
    /// passes it untouched — and that mutation wastes **102 GB** at 10000²
    /// growing from 129 slices to 130. The test whose whole purpose is "can
    /// this still overshoot" could not see an overshoot three orders of
    /// magnitude past its own bound.
    ///
    /// The range is `0..needed` and not `0..=MAX_SLOTS`, because that is the
    /// function's contract: `ensure_slots` returns early unless
    /// `needed > capacity`, so it is never called with `current >= needed`.
    /// Sweeping above `needed` would measure `current - needed` slices of an
    /// array that already exists and call the excess this function's fault.
    #[test]
    fn the_overshoot_is_bounded_by_the_budget_on_every_canvas() {
        let mut worst = 0u64;
        let mut worst_at = (0u64, 0u64, 0u32, 0u32);
        for (w, h) in CANVASES {
            let slice = w * h * LAYER_BYTES_PER_PIXEL;
            for needed in 1..=MAX_SLOTS as u32 {
                for current in 0..needed {
                    let cap = grown_capacity(current, needed, slice);
                    assert!(cap >= needed, "{w}x{h}: {current} -> {needed} gave {cap}");
                    let waste = u64::from(cap - needed) * slice;
                    assert!(
                        waste <= GROWTH_DOUBLING_BUDGET_BYTES,
                        "{w}x{h} growing {current} -> {needed} wasted {waste} bytes"
                    );
                    if waste > worst {
                        worst = waste;
                        worst_at = (w, h, current, needed);
                    }
                }
            }
        }
        // The figure quoted above, so it cannot drift from what is measured —
        // and *where*, because the worst case moved off the squares the moment
        // a non-square canvas was swept. 1024x512 needing a 129th slice, not
        // 1024² needing a 65th.
        assert_eq!(worst, 266_338_304);
        assert_eq!(worst_at, (1024, 512, 0, 129));
        // And a canvas whose slices are larger than the budget never
        // speculates at all, from any starting capacity.
        for needed in 1..=MAX_SLOTS as u32 {
            for current in 0..needed {
                assert_eq!(grown_capacity(current, needed, slice_of(10_000)), needed);
            }
        }
    }

    /// Total, and never short of what was asked for. The second is the one that
    /// would be a validation error rather than a waste.
    #[test]
    fn the_growth_rule_always_reaches_what_was_asked_for() {
        for (w, h) in CANVASES {
            let slice = w * h * LAYER_BYTES_PER_PIXEL;
            for needed in 1..=MAX_SLOTS as u32 {
                for current in [0u32, 1, 4, needed.saturating_sub(1)] {
                    let got = grown_capacity(current, needed, slice);
                    assert!(got >= needed, "{w}x{h}: {current} -> {needed} gave {got}");
                }
            }
        }
        // A degenerate canvas has no bytes to budget against and must still
        // terminate rather than spinning or overflowing.
        assert!(grown_capacity(1, 200, 0) >= 200);
    }

    #[test]
    fn a_band_is_the_whole_document_when_it_fits() {
        // The ordinary case, and the one that must not change: every readback
        // stays a single copy for every document a device can hold in one
        // buffer. 2048² at four bytes is 16 MB against a 256 MB limit.
        assert_eq!(band_rows(256 << 20, 2048 * 4, 2048), 2048);
    }

    #[test]
    fn a_document_larger_than_the_limit_is_split() {
        // The canvas that crashed: 10000², 40 KB a row, against 256 MB.
        let padded = 10_000 * 4;
        let band = band_rows(256 << 20, padded, 10_000);
        assert!(band < 10_000, "should have been split, got {band}");
        assert!(
            u64::from(band) * u64::from(padded) <= 256 << 20,
            "a band of {band} rows is over the limit"
        );
        // And the band after it must still reach the bottom of the document.
        assert!(band * 2 >= 10_000, "{band} rows would need three passes");
    }

    #[test]
    fn a_band_is_never_zero_rows() {
        // A row wider than the whole limit cannot be honoured, and returning
        // zero would be an infinite loop rather than a refusal. It takes a
        // canvas 67 million pixels across, which `max_texture_dimension_2d`
        // stops long before this is asked.
        assert_eq!(band_rows(16, 4096, 100), 1);
        assert_eq!(band_rows(0, 4, 100), 1);
    }

    #[test]
    fn a_band_never_overshoots_the_document() {
        // The copy's extent comes from this; a band taller than what is left
        // would name rows past the bottom of the texture, which is a validation
        // error and therefore an abort.
        assert_eq!(band_rows(u64::MAX, 256, 7), 7);
    }

    /// **The jump flood's seed target is a render attachment on every device the
    /// specification describes**, which is not something to take on trust.
    ///
    /// `guaranteed_format_features` is what the WebGPU spec requires of *any*
    /// adapter, as against `Adapter::get_texture_format_features`, which is what
    /// the one in front of you happens to offer. Asking the second would pass on
    /// this machine and say nothing about anybody else's, and the failure it hides
    /// is a `create_texture` validation error — fatal, because
    /// `crash::device_error` makes every uncaptured error fatal. Same shape as
    /// `the_slice_ceiling_agrees_with_umber_core`, and for the same reason.
    ///
    /// A test rather than a `const` assertion only because that method is not a
    /// `const fn`.
    #[test]
    fn the_seed_format_is_a_render_target_on_every_device() {
        let features = SEED_FORMAT.guaranteed_format_features(wgpu::Features::empty());
        assert!(
            features
                .allowed_usages
                .contains(wgpu::TextureUsages::RENDER_ATTACHMENT),
            "{SEED_FORMAT:?} is no longer a guaranteed render target: {:?}",
            features.allowed_usages
        );
        // And the coverage fields, which are the other half of the bake's own
        // allocations. `R8Unorm` is the stroke scratch's format too, so this is
        // already relied on elsewhere — stated here because the bake is what would
        // break next.
        assert!(
            STROKE_FORMAT
                .guaranteed_format_features(wgpu::Features::empty())
                .allowed_usages
                .contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
        );
    }

    /// **The pass budget covers every effect the model will let somebody
    /// install**, and the figure it is measured against is the worst one
    /// `plan_effect` can emit.
    ///
    /// Not a restatement of the constants: the count is derived here from the same
    /// arithmetic the planner uses — the flood's step count is
    /// `ceil(log2(span)) + 1` for a `span` the canvas bounds, plus the extract, the
    /// seed, the grow, the downsample, four box passes and the resolve — over the
    /// largest canvas `max_texture_dimension_2d` permits, which is the case the
    /// buffer has to be sized for and is not the case anybody measures.
    ///
    /// It is worth having because the first draft of this budget was **five times
    /// too small** and failed in the worst possible direction: overrunning it
    /// abandoned the whole bake, so no effect drew at all, every frame, while
    /// `effects_dropped` reported zero. A panel built on that figure would have
    /// said the document was within its budget while showing none of it.
    #[test]
    fn the_pass_budget_covers_the_effects_the_model_permits() {
        // **The span is the canvas's longest side, and not
        // `downlevel_defaults().max_texture_dimension_2d`.** `Gpu::new` asks for
        // `using_resolution`, which raises exactly that limit from the adapter —
        // so the downlevel figure of 2048 is what a canvas is *guaranteed* to
        // reach and says nothing about the largest one a device will allow. Using
        // it made this guard read 37 where the real worst is 45, which is the
        // difference between eleven passes of headroom and three.
        let worst_at = |longest: u32| {
            let span = longest.max(1);
            let mut floods = 0;
            let mut k = 1i64 << (32 - span.leading_zeros());
            while k >= 1 {
                floods += 1;
                k /= 2;
            }
            // **Two fields, because a centred outline is the worst case and it
            // floods twice.** The first draft counted one — the guard whose
            // comment claims more reach than the code has, since a `Centre`
            // position added to the planner afterwards would have been invisible
            // to it.
            let field = 1 + floods + 1; // seed, the flood steps, grow
            // extract + two fields + down + four box + resolve.
            1 + 2 * field + 1 + 4 + 1
        };

        // Past every `max_texture_dimension_2d` a device reports today: 16384 is
        // the common desktop figure, 32768 the generous one, and 65536 is over
        // both. A device beyond that overruns the per-effect budget and degrades
        // **visibly** rather than fatally — `bake_effects` keeps
        // `EFFECT_PASS_BLOCKS / EFFECT_MAX_PASSES_PER_EFFECT` effects and
        // `run_effect_steps` refuses a plan past the buffer, both counted in
        // `dropped` — which is why an upper bound here is a guard and not a
        // promise about hardware nobody has.
        for longest in [2048, 8192, 16384, 32768, 65536] {
            let worst = worst_at(longest);
            assert!(
                worst <= EFFECT_MAX_PASSES_PER_EFFECT,
                "at {longest} square one effect asks for {worst} passes against a \
                 budget of {EFFECT_MAX_PASSES_PER_EFFECT}"
            );
            assert!(
                EFFECT_PASS_BLOCKS >= (worst * umber_core::effect::MAX_ENABLED) as u64,
                "{} blocks will not hold {} effects at {worst} passes each",
                EFFECT_PASS_BLOCKS,
                umber_core::effect::MAX_ENABLED
            );
        }
        // And the buffer's own size, because a dynamic offset past the end of it is
        // a validation error and therefore fatal.
        let last = (EFFECT_PASS_BLOCKS - 1) * EFFECT_BLOCK_STRIDE;
        assert!(
            last + std::mem::size_of::<EffectUniforms>() as u64
                <= EFFECT_PASS_BLOCKS * EFFECT_BLOCK_STRIDE
        );
    }

    /// **Above the threshold the live gate is per effect, and it agrees with the
    /// planner because both read one function.**
    ///
    /// The rule the canvas-wide gate was replaced by, and the replacement is what
    /// stops one expensive outline anywhere in the stack switching the live rebake
    /// off for a cheap shadow on another layer. Below the threshold everything is
    /// live, which is the case that must not have been narrowed by accident.
    ///
    /// The last block is the one worth having: `plan_effect`'s own `grow` is
    /// `effect_field(effect).reach > 0.0`, the exact negation of the gate's second
    /// clause, so an effect cannot be judged cheap and then plan a flood —
    /// asserted rather than argued, over every kind and position.
    ///
    /// **A blur clause was tried here and removed**, and the case that killed it is
    /// asserted below so it cannot come back: the default 5 px shadow blurs at
    /// *full* resolution, and it is the cheapest bake there is.
    #[test]
    fn the_live_gate_admits_exactly_the_bakes_that_fit() {
        let small = EFFECT_LIVE_PIXELS;
        let large = EFFECT_LIVE_PIXELS + 1;

        let shadow = Effect::drop_shadow();
        let wide = Effect {
            softness: 64.0,
            ..Effect::drop_shadow()
        };
        let spread = Effect {
            spread: 8.0,
            ..Effect::drop_shadow()
        };
        let outside = Effect {
            spread: 16.0,
            position: OutlinePosition::Outside,
            ..Effect::outline()
        };
        let centre = Effect {
            position: OutlinePosition::Centre,
            ..outside
        };

        // A small canvas takes every one of them live.
        for effect in [shadow, wide, spread, outside, centre] {
            assert!(
                effect_bakes_live_at(small, &effect),
                "{effect:?} is not live on a canvas inside the threshold"
            );
        }

        // A large one takes only the two that need no distance field and whose
        // blur is absent or downsampled.
        assert!(effect_bakes_live_at(large, &shadow), "a 5 px shadow");
        assert!(effect_bakes_live_at(large, &wide), "a 64 px shadow");
        for effect in [spread, outside, centre] {
            assert!(
                !effect_bakes_live_at(large, &effect),
                "{effect:?} floods and must not be rebaked every frame"
            );
        }

        // **The case that killed the blur clause.** A default drop shadow blurs at
        // *full* resolution — its softness is 5 and the threshold is 32 — and it is
        // the cheapest bake there is, 7.6 ms at 10000². A rule reading "downsampled
        // or absent" sounds like it names the cheap blurs and refuses this one
        // while admitting a 64 px shadow, which is nine times its radius.
        assert_eq!(tent_for(shadow.softness).0, 1, "the default blurs whole");
        assert!(
            effect_bakes_live_at(large, &shadow),
            "the cheapest bake there is must not be refused for blurring at full \
             resolution"
        );

        // And a wide full-resolution blur *is* live, which is the known worst this
        // gate accepts: around 31 px of softness is a box radius of 15 and 22.5 ms
        // at 10000², over a 60 Hz frame on its own. Asserted so the cost is
        // recorded rather than discovered.
        let mid = Effect {
            softness: EFFECT_FULL_RES_SOFTNESS - 1.0,
            ..Effect::drop_shadow()
        };
        assert_eq!(tent_for(mid.softness), (1, 15));
        assert!(effect_bakes_live_at(large, &mid));

        // **The gate and the planner read one function.** `plan_effect` decides
        // whether to flood with `effect_field(..).reach > 0.0`, which is the exact
        // negation of this gate's first clause — so "cheap" and "plans no flood"
        // cannot come apart.
        for effect in [shadow, wide, spread, outside, centre] {
            let floods = effect_field(&effect).reach > 0.0;
            let cheap_enough = effect_bakes_live_at(large, &effect);
            assert!(
                !(floods && cheap_enough),
                "{effect:?} would flood and was judged affordable anyway"
            );
        }
    }

    /// A softness of zero records **no blur pass**, which is the exact identity
    /// the selection's feather and the brush's grain both keep — and the half of
    /// "an effect with no reach is the identity" that is arithmetic rather than a
    /// decision.
    #[test]
    fn no_softness_is_no_blur_pass_at_all() {
        assert_eq!(tent_for(0.0), (1, 0));
        assert_eq!(tent_for(-3.0), (1, 0));
        assert!(tent_for(1.0).1 > 0, "a real softness must blur something");
    }

    /// The tent the blur runs is the tent that was asked for, to within the step
    /// its own kernel can express.
    ///
    /// Two discrete box passes of `2r + 1` taps at a reduction of `d` have a half
    /// support of `d(2r + 1)`, so the error is bounded by `d` — which is the whole
    /// argument for [`EFFECT_FULL_RES_SOFTNESS`] and the reason that constant is a
    /// parameter rather than a second implementation. **A pixel below the
    /// threshold and thirteen per cent above it**; the assertion is written that
    /// way round because lowering the threshold is the tempting change and it
    /// makes the *narrow* end wrong, which is the end a painter dials.
    #[test]
    fn the_tent_is_the_width_it_was_asked_for() {
        for asked in 1..=200 {
            let softness = asked as f32;
            let (down, radius) = tent_for(softness);
            let support = down as f32 * (2 * radius + 1) as f32;
            let slack = if softness < EFFECT_FULL_RES_SOFTNESS {
                // A full-resolution tent's step is two texels, and the smallest
                // it can express at all is three — so a softness of one and of
                // two both come out at three.
                3.0
            } else {
                0.13 * softness
            };
            assert!(
                (support - softness).abs() <= slack,
                "asked {softness}, got {support} from down {down} radius {radius}"
            );
        }
    }

    /// A softness the quarter-resolution kernel could not represent runs at full
    /// resolution instead.
    ///
    /// The departure from §3.2 stated as a test rather than as a sentence: below
    /// the threshold the reduction is 1, above it 4, and the point of the second
    /// assertion is that the *cheap* path really is taken where it can be. A
    /// change that took the reduction to 4 everywhere would fail the width test
    /// above at the narrow end; a change that took it to 1 everywhere would pass
    /// every assertion here except this one, and cost 83 ms a frame at 10000².
    #[test]
    fn a_narrow_soft_edge_is_blurred_at_full_resolution() {
        assert_eq!(tent_for(EFFECT_FULL_RES_SOFTNESS - 1.0).0, 1);
        assert_eq!(tent_for(EFFECT_FULL_RES_SOFTNESS).0, EFFECT_DOWN);
        assert_eq!(tent_for(64.0).0, EFFECT_DOWN);
    }

    /// Which effects mark nothing, in the two forms the rule takes.
    ///
    /// The outline half is arithmetic — a band of no width is no band. The shadow
    /// half is a decision and it is argued at [`effect_marks_nothing`]; what is
    /// pinned here is that it is narrow, because widening it by one clause would
    /// silently stop drawing a shadow somebody had dialled. A displacement alone
    /// is enough, a spread alone is enough, and a softness alone is enough.
    #[test]
    fn only_an_effect_with_no_reach_marks_nothing() {
        let shadow = Effect {
            spread: 0.0,
            softness: 0.0,
            distance: 0.0,
            ..Effect::drop_shadow()
        };
        assert!(effect_marks_nothing(&shadow));
        assert!(!effect_marks_nothing(&Effect {
            distance: 4.0,
            ..shadow
        }));
        assert!(!effect_marks_nothing(&Effect {
            spread: 1.0,
            ..shadow
        }));
        assert!(!effect_marks_nothing(&Effect {
            softness: 1.0,
            ..shadow
        }));

        // An outline is its width and nothing else, at every position.
        for position in OutlinePosition::ALL {
            let bare = Effect {
                spread: 0.0,
                position,
                ..Effect::outline()
            };
            assert!(effect_marks_nothing(&bare), "{position:?}");
            assert!(
                !effect_marks_nothing(&Effect {
                    spread: 2.0,
                    ..bare
                }),
                "{position:?}"
            );
        }

        // And the two that hold whatever the kind: off, and transparent.
        for kind in EffectKind::ALL {
            let live = Effect {
                spread: 5.0,
                softness: 5.0,
                distance: 5.0,
                ..Effect::of(kind)
            };
            assert!(!effect_marks_nothing(&live), "{kind:?}");
            assert!(effect_marks_nothing(&Effect {
                enabled: false,
                ..live
            }));
            assert!(effect_marks_nothing(&Effect {
                opacity: 0.0,
                ..live
            }));
        }
    }

    /// The text budget is read off the **area** and not off either side of it,
    /// and a block at its placed size is nowhere near it.
    ///
    /// Both halves are needed and the first is the one that would go wrong
    /// quietly. A rule written against `width` and `height` separately — the
    /// obvious spelling, and the one a rectangle invites — admits a 4000×200
    /// block, which is 0.8 megapixels and about 6 ms, while refusing a 1100×900
    /// one that is the same work; and it *is* the shape a drag produces, because
    /// scaling a line of text grows one axis far faster than the other. The
    /// second half is the claim [`TEXT_RESET_LIVE_PIXELS`] makes about ordinary
    /// text, stated in the units `measure-text.rs` prints so the two cannot
    /// drift: a caption is ninety times under the budget and a paragraph is six.
    #[test]
    fn the_text_reset_budget_is_an_area_and_an_ordinary_caption_is_far_inside_it() {
        let rect = |width, height| PixelRect {
            x: 0,
            y: 0,
            width,
            height,
        };
        // Measured destinations, out of `measure-text.rs`.
        assert!(text_reset_is_live(rect(212, 54)), "a placed caption");
        assert!(text_reset_is_live(rect(747, 223)), "a placed paragraph");
        assert!(
            text_reset_is_live(rect(848, 213)),
            "a caption dragged to 4x"
        );
        assert!(
            !text_reset_is_live(rect(3390, 848)),
            "a caption dragged to 16x is 20 ms and must not be live"
        );
        assert!(
            !text_reset_is_live(rect(2988, 890)),
            "a paragraph dragged to 4x is 17 ms and must not be live"
        );
        // The area and not the extents: a long thin block and a squarer one of
        // the same area get the same answer, in both directions across the line.
        assert!(text_reset_is_live(rect(4000, 200)) == text_reset_is_live(rect(1000, 800)));
        assert!(!text_reset_is_live(rect(8000, 200)));
        assert!(!text_reset_is_live(rect(1500, 1100)));
        // Exactly at the budget is live; one pixel past it is not.
        assert!(text_reset_is_live(rect(1024, 1024)));
        assert!(!text_reset_is_live(rect(1025, 1024)));
    }

    /// The parameter hash covers what the **pixels** depend on and nothing else.
    ///
    /// The direction that matters is the second half: `opacity` and `blend` are
    /// the draw's, applied by `composite.wgsl`, so dragging either must not
    /// rebake — a canvas-sized bake per frame of a slider drag is exactly the cost
    /// the cache exists to avoid. Property-driven over every field, because a
    /// field missed is silent and looks like a driver bug.
    #[test]
    fn the_parameter_hash_reads_every_field_the_pixels_depend_on() {
        let base = Effect {
            spread: 3.0,
            softness: 4.0,
            angle: 30.0,
            distance: 5.0,
            ..Effect::drop_shadow()
        };
        let h = effect_params_hash(&base);

        let moved: [Effect; 6] = [
            Effect {
                spread: 3.5,
                ..base
            },
            Effect {
                softness: 4.5,
                ..base
            },
            Effect {
                angle: 31.0,
                ..base
            },
            Effect {
                distance: 5.5,
                ..base
            },
            Effect {
                color: Color::new(0.1, 0.2, 0.3, 0.4),
                ..base
            },
            Effect {
                kind: EffectKind::Outline,
                ..base
            },
        ];
        for other in moved {
            assert_ne!(
                effect_params_hash(&other),
                h,
                "a parameter the pixels depend on is not in the hash: {other:?}"
            );
        }
        // An outline's position changes which side of the edge the band is on and
        // therefore which flood is run, so it is in the hash too.
        assert_ne!(
            effect_params_hash(&Effect {
                kind: EffectKind::Outline,
                position: OutlinePosition::Inside,
                ..base
            }),
            effect_params_hash(&Effect {
                kind: EffectKind::Outline,
                position: OutlinePosition::Outside,
                ..base
            })
        );

        for same in [
            Effect {
                opacity: 0.25,
                ..base
            },
            Effect {
                blend: BlendMode::Screen,
                ..base
            },
        ] {
            assert_eq!(
                effect_params_hash(&same),
                h,
                "the draw's own settings must not rebake: {same:?}"
            );
        }
    }
}
