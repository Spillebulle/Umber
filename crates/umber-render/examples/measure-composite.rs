//! What the tile atlas costs the composite, measured against the dense slice it
//! replaced.
//!
//! ```sh
//! cargo run --release -p umber-render --example measure-composite
//! cargo run --release -p umber-render --example measure-composite -- --sizes 4096x4096 --budget 5
//! cargo run --release -p umber-render --example measure-composite -- --repeat 3
//! cargo run --release -p umber-render --example measure-composite -- --fallback
//! ```
//!
//! `measure-vram` answers the memory question — a 20000x5000 document went from
//! 19.74 GB of layer array to 1.54 GB and it opens. This is the other half, and
//! `docs/perf/roadmap.md` Stage 6 parks four items behind it: the composite's
//! fragment shader now does **one page-table load plus up to four atlas
//! `textureLoad`s** per layer per fragment where it used to do one hardware
//! bilinear sample, and those are a *dependent* chain — the table read has to
//! come home before the taps can be issued. That is a latency question as much
//! as a bandwidth one, and nothing in the design measured it.
//!
//! # The A/B, and why it is one shader rather than two
//!
//! `docs/perf/composite-throughput.md` §8.1 specifies it: build a second
//! pipeline from `tiles.wgsl` with `tile_bilinear` replaced by
//! `textureSampleLevel(atlas, samp, doc / page_size, slot, 0)`. **Under the
//! identity page table that substitution is exactly correct** — a page is the
//! canvas rounded up to whole tiles and the identity layout puts slot `s`'s
//! tiles at page `s`, cell `(tx, ty)`, so page slice `s` *is* the dense
//! canvas-sized slice the atlas replaced. Same shader, same data, same bind
//! group layout, same uniform, one function body different.
//!
//! Everything else is held equal on purpose. The substitution is textual and
//! **self-checking**: it takes `tiles.wgsl` up to `fn tile_bilinear(` and
//! refuses to run if that is not the last item in the file, so a function added
//! after it cannot be silently dropped from the variant. Both variants declare
//! the same extra sampler at the same binding whether or not they read it, so
//! the two pipelines share one layout and differ in nothing else.
//!
//! Three variants are compiled, not two:
//!
//! - **tiled** — the shipped `tile_bilinear`, byte for byte.
//! - **sampled** — one hardware bilinear tap. This is the pre-atlas composite.
//! - **table** — the page-table load and the unbacked branch, returning without
//!   reading the atlas at all. Its *picture is meaningless* and only its timing
//!   is: it splits the atlas's cost into the dependent table read and the taps
//!   that hang off it, which is the split the design's argument turns on. It
//!   cannot be optimised away because it really does read a texture.
//!
//! # Residency is the axis that can reverse the sign
//!
//! An unbacked tile returns the slot's empty value and issues **no atlas
//! fetch**, where a dense slice is sampled whatever is in it — the composite
//! loop has no alpha early-out, so the sampled path's cost is independent of
//! what the slice holds. Across the artist's 33 real documents only 13.5% of
//! tiles are backed. So the atlas may be *faster* than the dense path on a real
//! document even where it is slower on a full one, and those are two different
//! answers that both matter.
//!
//! Three residency shapes, because the shape matters as much as the fraction:
//!
//! - **dense** — every tile backed, identity layout. The worst case for the
//!   atlas and the case somebody painting a background fill is in.
//! - **blob** — each slot backs a contiguous rectangle of about 13.5% of the
//!   canvas, placed differently per slot. This is what a real layer looks like:
//!   a character on part of the page.
//! - **scatter** — 13.5% of tiles chosen at random per slot. The adversarial
//!   shape, where the unbacked branch diverges within every warp.
//!
//! The blob and the scatter read a **packed** atlas — tiles compacted into the
//! pages they actually need, which is the production layout — so those columns
//! measure the real thing rather than a dense allocation with holes punched in
//! its table.
//!
//! # What is timed, and what that is worth
//!
//! Wall clock around `queue.submit` plus a blocking `poll`, median of many
//! samples after a warm-up. **Not** a GPU timestamp: `Features::TIMESTAMP_QUERY`
//! is not among the features Umber requests, so asking for it would measure a
//! device Umber never creates — `measure-effects.rs`'s stated reason, and
//! `composite-throughput.md` §8.1 repeats it. It over-estimates by the submit
//! and the fence, which is the safe direction.
//!
//! Several composite passes go in one encoder and the time is divided by the
//! count, so the fence is amortised rather than counted once per pass. The
//! **noise floor** is printed first and is the same submit-and-wait with an
//! empty encoder: any figure near it is a figure this instrument cannot
//! resolve, and the report says so rather than quoting it.
//!
//! Read `--repeat` before believing anything here. `measure-clipboard.rs`
//! records figures that went into the docs three times too slow because the
//! machine was building six other things; the remedy is to run the sweep more
//! than once and compare, which is what that flag is for. The spread is printed
//! beside every median for the same reason.
//!
//! # What is checked rather than assumed
//!
//! The dense cell renders once through each of the two real variants and the
//! frames are compared. They are two renderings of one picture — a hardware
//! bilinear tap and a hand lerp of four `textureLoad`s — so they agree to
//! within the last bit rather than exactly, and the largest deviation is
//! printed. A large one would mean the A/B is comparing two different pictures
//! and the whole table is void.
//!
//! It is an example rather than a test because it asserts wall-clock time,
//! which CLAUDE.md forbids on CI, and because it wants gigabytes of a real card.

use std::time::Instant;

use glam::{UVec2, Vec2};
use umber_core::camera::Camera;
use umber_core::tile::{Entry, Grid, TILE};
use umber_render::gpu::{Choice, Gpu};

// ---------------------------------------------------------------------------
// The shader, and the one substitution
// ---------------------------------------------------------------------------

const TILES: &str = include_str!("../shaders/tiles.wgsl");
const BLEND: &str = include_str!("../shaders/blend.wgsl");
const COMPOSITE: &str = include_str!("../shaders/composite.wgsl");

/// A binding every variant declares and only some read.
///
/// The sampled path needs a sampler and `composite.wgsl` declares its own at
/// binding 3 — after this file in the concatenation, so reading that one would
/// be a forward reference. Declaring a second here, in the text **all** variants
/// get, is what keeps the pipelines on one bind group layout and the difference
/// between them down to a single function body. An unused binding costs nothing
/// per fragment.
///
/// **Binding 7, not 6.** `composite.wgsl` took binding 6 for `mask_tex` — the
/// raw, non-sRGB view of the same atlas a mask is read through — on a branch
/// that landed beside the one this example was written on, and neither branch
/// could see the other. The result was a duplicate binding that failed pipeline
/// creation on the first line of the sweep, so this example did not run at all
/// between those two merges. That is the "wrong in the combination" failure
/// CLAUDE.md records, arriving through a binding number.
const EXTRA_BINDINGS: &str = "\n@group(0) @binding(7) var measure_samp: sampler;\n";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Variant {
    /// The shipped `tile_bilinear`.
    Tiled,
    /// One hardware bilinear tap — the pre-atlas composite.
    Sampled,
    /// The page-table load alone, no atlas read. Timing only; see the module
    /// docs.
    Table,
    /// `textureGather` inside the existing single-tile fast path: four gathers
    /// where the shipped path does four `textureLoad`s, and the identical hand
    /// lerp on the identical texel values. See [`GATHER_BODY`].
    Gather,
}

impl Variant {
    const ALL: [Variant; 4] = [
        Variant::Tiled,
        Variant::Sampled,
        Variant::Table,
        Variant::Gather,
    ];

    fn label(self) -> &'static str {
        match self {
            Variant::Tiled => "tiled",
            Variant::Sampled => "sampled",
            Variant::Table => "table",
            Variant::Gather => "gather",
        }
    }
}

/// `tile_bilinear` with the fast path's four `textureLoad`s replaced by four
/// `textureGather`s.
///
/// **The straddling path is byte for byte the shipped one.** A gather takes the
/// four texels of one bilinear footprint out of one texture, and a straddling
/// tap's four texels are in up to four different atlas cells, so there is
/// nothing there for a gather to do. That is exactly why the fast path is the
/// place to ask the question: `tiles.wgsl` records that a tap straddles a tile
/// boundary only within half a texel of one, `1 - (255/256)^2`, about 0.78%.
///
/// Three things had to be got right, and each of them would have been a picture
/// that moved:
///
/// **Where the gather is aimed.** The hardware picks its four texels as
/// `floor(uv * dim - 0.5)` and that one plus one, in its own fixed point with
/// a few bits of subtexel precision. Aiming at the true fractional position
/// would put it a rounding error away from a texel boundary and let it step
/// into the neighbouring tile — the seam this whole design exists to avoid. So
/// it is aimed at `(a + 1) / dim`, which lands `p` at exactly `a + 0.5`: half a
/// texel from either boundary, two orders of magnitude more slack than the
/// hardware's precision. `dim` is derived from `doc_size` rather than asked of
/// the texture, because a page *is* the canvas rounded up to whole tiles —
/// `Grid::page_size` — so it is three integer operations and no query.
///
/// **The component order.** WGSL follows D3D and Vulkan: the returned vector is
/// the four texels counter-clockwise from the lower left, which in a y-down
/// texture is `.w` = (0,0), `.z` = (1,0), `.x` = (0,1), `.y` = (1,1). Getting
/// this wrong is a picture shifted by a texel, which is why the check below
/// demands *exact* equality against the shipped path rather than a tolerance.
///
/// **The clamp at the canvas edge.** The shipped path clamps both taps into the
/// canvas, so within half a texel of the document's own border the two taps are
/// one texel and it lerps a value against itself. A gather cannot be clamped
/// that way — it fetches `a` and `a + 1` whatever they are, and `a + 1` there is
/// either a real canvas texel (left edge) or the page's padding (right edge).
/// Zeroing the weight on any axis where the two taps collapsed is what makes
/// that exactly equal and not merely close: `mix(x, y, 0.0)` is `x * 1 + y * 0`,
/// which is `x` for any finite `y`, and an 8-bit unorm texel is always finite.
/// It is a `select` rather than a branch, so the common path pays two
/// comparisons and no divergence.
const GATHER_BODY: &str = r#"
fn tile_bilinear(
    atlas: texture_2d_array<f32>,
    table: texture_2d_array<u32>,
    slot: i32,
    doc: vec2<f32>,
    doc_size: vec2<i32>,
    empty: vec4<f32>,
) -> vec4<f32> {
    let centred = doc - vec2<f32>(0.5);
    let base = floor(centred);
    let w = centred - base;
    let hi = doc_size - vec2<i32>(1);
    let lo = clamp(vec2<i32>(base), vec2<i32>(0), hi);
    let up = clamp(vec2<i32>(base) + vec2<i32>(1), vec2<i32>(0), hi);

    let t_lo = lo / TILE;
    let t_up = up / TILE;
    if (t_lo.x == t_up.x && t_lo.y == t_up.y) {
        let entry = textureLoad(table, t_lo, slot, 0).r;
        if (entry == TILE_UNBACKED) {
            return empty;
        }
        let at = tile_atlas_texel(entry, lo, t_lo);
        let page = i32(entry >> TILE_PAGE_SHIFT);
        // The page is the canvas rounded up to whole tiles: Grid::page_size.
        let dim = vec2<f32>(((doc_size + TILE - 1) / TILE) * TILE);
        let uv = (vec2<f32>(at) + vec2<f32>(1.0)) / dim;
        // Where a tap was clamped into the canvas the two taps are one texel,
        // so this weight is zero and whatever the gather picked up beside it is
        // multiplied by zero. Exactly what the four-load path computes.
        let ww = select(w, vec2<f32>(0.0), lo == up);
        let gr = textureGather(0, atlas, measure_samp, uv, page);
        let gg = textureGather(1, atlas, measure_samp, uv, page);
        let gb = textureGather(2, atlas, measure_samp, uv, page);
        let ga = textureGather(3, atlas, measure_samp, uv, page);
        let c00 = vec4<f32>(gr.w, gg.w, gb.w, ga.w);
        let c10 = vec4<f32>(gr.z, gg.z, gb.z, ga.z);
        let c01 = vec4<f32>(gr.x, gg.x, gb.x, ga.x);
        let c11 = vec4<f32>(gr.y, gg.y, gb.y, ga.y);
        return mix(mix(c00, c10, ww.x), mix(c01, c11, ww.x), ww.y);
    }

    let c00 = tile_load(atlas, table, slot, vec2<i32>(lo.x, lo.y), empty);
    let c10 = tile_load(atlas, table, slot, vec2<i32>(up.x, lo.y), empty);
    let c01 = tile_load(atlas, table, slot, vec2<i32>(lo.x, up.y), empty);
    let c11 = tile_load(atlas, table, slot, vec2<i32>(up.x, up.y), empty);
    return mix(mix(c00, c10, w.x), mix(c01, c11, w.x), w.y);
}
"#;

/// `tiles.wgsl` up to `fn tile_bilinear(`, having checked that nothing follows
/// it.
///
/// The check is the point. Taking a prefix of a file and appending a
/// replacement silently drops anything the file grew after the marker, and the
/// symptom would be a shader that no longer compiles — or worse, one that does.
fn tiles_head() -> &'static str {
    let cut = TILES
        .find("fn tile_bilinear(")
        .expect("tiles.wgsl no longer declares tile_bilinear; this example substitutes for it");
    let after = &TILES[cut + 1..];
    assert!(
        !after.contains("\nfn "),
        "tile_bilinear is no longer the last function in tiles.wgsl, so taking \
         the text before it would drop whatever follows. Move the substitution."
    );
    &TILES[..cut]
}

/// The whole composite shader for one variant, at one page size.
///
/// `page` reaches the sampled body as a compile-time constant rather than
/// through the uniform, which is the *favourable* reading for the baseline: it
/// pays no uniform read the tiled path does not also pay. Understating the
/// atlas's cost is the direction that would make this measurement worthless, so
/// every choice here goes the other way.
fn shader_source(variant: Variant, page: UVec2) -> String {
    let body = match variant {
        Variant::Tiled => TILES[TILES.find("fn tile_bilinear(").unwrap()..].to_string(),
        Variant::Sampled => format!(
            "fn tile_bilinear(\n\
             \x20   atlas: texture_2d_array<f32>,\n\
             \x20   table: texture_2d_array<u32>,\n\
             \x20   slot: i32,\n\
             \x20   doc: vec2<f32>,\n\
             \x20   doc_size: vec2<i32>,\n\
             \x20   empty: vec4<f32>,\n\
             ) -> vec4<f32> {{\n\
             \x20   return textureSampleLevel(\n\
             \x20       atlas, measure_samp, doc / vec2<f32>({:.1}, {:.1}), slot, 0.0\n\
             \x20   );\n\
             }}\n",
            page.x as f32, page.y as f32
        ),
        // Reads the page table and the unbacked branch, and stops there. The
        // return depends on the fetch so nothing can fold it away.
        Variant::Table => "fn tile_bilinear(\n\
             \x20   atlas: texture_2d_array<f32>,\n\
             \x20   table: texture_2d_array<u32>,\n\
             \x20   slot: i32,\n\
             \x20   doc: vec2<f32>,\n\
             \x20   doc_size: vec2<i32>,\n\
             \x20   empty: vec4<f32>,\n\
             ) -> vec4<f32> {\n\
             \x20   let hi = doc_size - vec2<i32>(1);\n\
             \x20   let lo = clamp(vec2<i32>(doc), vec2<i32>(0), hi);\n\
             \x20   let entry = textureLoad(table, lo / TILE, slot, 0).r;\n\
             \x20   if (entry == TILE_UNBACKED) { return empty; }\n\
             \x20   return vec4<f32>(f32(entry & 255u) * (0.25 / 255.0));\n\
             }\n"
        .to_string(),
        Variant::Gather => GATHER_BODY.to_string(),
    };
    format!(
        "{}{}{}{}{}",
        tiles_head(),
        EXTRA_BINDINGS,
        body,
        BLEND,
        COMPOSITE
    )
}

// ---------------------------------------------------------------------------
// The uniform, mirroring `ViewUniforms` in `canvas.rs` byte for byte
// ---------------------------------------------------------------------------

/// `MAX_DRAWS` in `canvas.rs` and in `composite.wgsl`. Restated here because
/// neither is public, and `the_view_uniform_is_the_size_canvas_writes` below
/// would catch a divergence at the first run rather than silently reading past
/// the block.
const MAX_DRAWS: usize = 191;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ViewUniforms {
    scale: [f32; 2],
    offset: [f32; 2],
    doc_size: [f32; 2],
    pivot: [f32; 2],
    stroke_color: [f32; 4],
    backdrop: [f32; 4],
    background: [f32; 4],
    layer_count: u32,
    stroke_mode: u32,
    active_index: u32,
    checker: f32,
    is_export: u32,
    per_dab_color: u32,
    stroke_on_mask: u32,
    stroke_blend: u32,
    layers: [[f32; 4]; MAX_DRAWS],
    extra: [[f32; 4]; MAX_DRAWS],
}

/// One frame's worth of draws, all Normal, all visible.
fn view_uniforms(
    doc: UVec2,
    camera: &Camera,
    pivot: Vec2,
    layers: u32,
    masks: bool,
) -> ViewUniforms {
    let scale = 1.0 / camera.zoom;
    let offset = camera.center - pivot * scale;
    let mut packed = [[0.0f32; 4]; MAX_DRAWS];
    let mut extra = [[0.0f32; 4]; MAX_DRAWS];
    for i in 0..layers as usize {
        // (opacity, blend, slot, visible). A little under one so the stack
        // genuinely accumulates rather than the first opaque layer answering
        // for the rest — though nothing in the loop branches on that.
        packed[i] = [0.75, 0.0, i as f32, 1.0];
        // A masked layer takes a second tap of the atlas, on another slot.
        // Slot 0 stands in for the mask; which slice it is does not change the
        // fetch count and every slot is resident in every store here.
        extra[i] = [
            if masks { 0.0 } else { i as f32 },
            if masks { 1.0 } else { 0.0 },
            0.0,
            0.0,
        ];
    }
    ViewUniforms {
        scale: [scale, scale],
        offset: [offset.x, offset.y],
        doc_size: [doc.x as f32, doc.y as f32],
        pivot: [pivot.x, pivot.y],
        stroke_color: [0.0, 0.0, 0.0, 0.0],
        backdrop: [0.1, 0.1, 0.1, 1.0],
        background: [0.0, 0.0, 0.0, 0.0],
        layer_count: layers,
        stroke_mode: 0,
        // No layer takes the stroke, which is an idle frame — the frame a pan
        // or a zoom is made of, and the one the parked items are about.
        active_index: u32::MAX,
        checker: 8.0,
        // The screen path, not the export one: the checkerboard and the sRGB
        // encode are part of what a frame pays.
        is_export: 0,
        per_dab_color: 0,
        stroke_on_mask: 0,
        stroke_blend: 0,
        layers: packed,
        extra,
    }
}

// ---------------------------------------------------------------------------
// Residency
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Residency {
    /// Every tile backed, identity layout, in the dense atlas.
    Dense,
    /// A contiguous rectangle of about `FRACTION` of the canvas per slot.
    Blob,
    /// `FRACTION` of tiles scattered at random per slot.
    Scatter,
}

impl Residency {
    const ALL: [Residency; 3] = [Residency::Dense, Residency::Blob, Residency::Scatter];

    fn label(self) -> &'static str {
        match self {
            Residency::Dense => "dense",
            Residency::Blob => "blob",
            Residency::Scatter => "scatter",
        }
    }
}

/// What `survey-residency` measured across the artist's 33 documents.
const FRACTION: f64 = 0.135;

/// A small deterministic generator. Seeded per slot so a run reproduces, for
/// the reason the scatter RNG in `StrokeBuilder` is seeded per stroke.
fn next(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// Which of a slot's tiles are backed, as a row-major bitmap over the grid.
fn backed_tiles(grid: &Grid, slot: u32, residency: Residency) -> Vec<bool> {
    let (tx, ty) = (grid.tiles.x, grid.tiles.y);
    let total = (tx * ty) as usize;
    let mut out = vec![false; total];
    match residency {
        Residency::Dense => out.fill(true),
        Residency::Blob => {
            // A rectangle of about FRACTION of the area, roughly square, placed
            // somewhere different on every slot. At least one tile, because a
            // layer holding nothing at all is a layer the composite would be
            // measuring the absence of.
            let want = ((total as f64) * FRACTION).round().max(1.0) as u32;
            let side = (want as f64).sqrt().ceil() as u32;
            let w = side.min(tx).max(1);
            let h = want.div_ceil(w).min(ty).max(1);
            let mut state = 0x9E37_79B9_7F4A_7C15 ^ u64::from(slot).wrapping_mul(0x2545_F491);
            let ox = (next(&mut state) % u64::from(tx - w + 1)) as u32;
            let oy = (next(&mut state) % u64::from(ty - h + 1)) as u32;
            for y in oy..oy + h {
                for x in ox..ox + w {
                    out[(y * tx + x) as usize] = true;
                }
            }
        }
        Residency::Scatter => {
            let want = ((total as f64) * FRACTION).round().max(1.0) as usize;
            let mut state = 0xDEAD_BEEF_CAFE_F00D ^ u64::from(slot).wrapping_mul(0x9E37_79B9);
            let mut placed = 0;
            while placed < want {
                let i = (next(&mut state) % total as u64) as usize;
                if !out[i] {
                    out[i] = true;
                    placed += 1;
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The atlas and its page table
// ---------------------------------------------------------------------------

/// One atlas and one page table over it: what a bind group is built from.
struct Store {
    atlas_view: wgpu::TextureView,
    /// The same texture without the transfer function, which is what
    /// `composite.wgsl` binds at 6 and reads a mask through. Same fetch, same
    /// cost; it is here so the pipeline layout is the shipped one.
    raw_view: wgpu::TextureView,
    table_view: wgpu::TextureView,
    pages: u32,
    backed: u64,
    bytes: u64,
}

const LAYER_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// `LAYER_FORMAT_LINEAR` in `canvas.rs`. See [`Store::raw_view`].
const LAYER_FORMAT_LINEAR: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const PAGE_TABLE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Uint;
const STROKE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Unorm;
/// What the real surface is. The screen pipeline is compiled against the
/// swapchain's format, so this is the shipped configuration rather than
/// `OFFSCREEN_FORMAT`, which only the export path uses.
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

/// How much staging one `write_texture` of the atlas may ask for. See the
/// banding in `build_store` for why a whole page is too much.
const UPLOAD_BAND_BYTES: u64 = 16 << 20;

/// One page's worth of premultiplied paint, the same for every page.
///
/// The content is deliberately the same on every page and every slot: nothing
/// in the composite loop branches on a texel's value — there is no alpha
/// early-out, which is `composite-throughput.md` §3.1's R1 — so what a slice
/// holds cannot change what it costs to read. What *does* change the cost is
/// whether a tile is backed at all, and that lives in the page table.
///
/// **Every texel differs from its neighbours, and that is for the check rather
/// than for the timing.** The first draft varied on a four-texel block, so
/// neighbouring texels were usually equal, the bilinear weights had nothing to
/// act on, and tiled and sampled agreed exactly while saying nothing whatever
/// about the hand lerp. A guard that agrees for the wrong reason is the failure
/// this codebase records most often; here the remedy is one line of fill.
///
/// Alpha is held at 64 so that the colour stays a legal premultiplied value and
/// so a stack of 54 accumulates rather than saturating at the second layer.
///
/// **The padding outside the canvas replicates the edge texel, and the check is
/// what found that it had to.** A page is the canvas rounded up to whole tiles,
/// so 1920x1080 is stored in 2048x1280 — and the sampled baseline's
/// `ClampToEdge` clamps at the *page's* edge, 2047, where `tile_bilinear`
/// clamps at the canvas's, 1919. With independent content in the padding the
/// two disagreed by **9 of 255** along the last half-texel band, which is not a
/// bug in either path: it is the baseline being unfaithful to the
/// canvas-*sized* slice the atlas replaced, whose clamp had nothing beyond the
/// canvas to reach. Replicating the edge makes the page's clamp and the
/// canvas's agree by construction.
fn page_bytes(page: UVec2, doc: UVec2) -> Vec<u8> {
    let mut out = vec![0u8; (page.x as usize) * (page.y as usize) * 4];
    for (i, px) in out.chunks_exact_mut(4).enumerate() {
        let x = (i % page.x as usize) as u32;
        let y = (i / page.x as usize) as u32;
        let (x, y) = (x.min(doc.x - 1), y.min(doc.y - 1));
        let h = x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(2_246_822_519);
        let a = 64u8;
        px.copy_from_slice(
            [
                (h % 65) as u8,
                ((h >> 8) % 65) as u8,
                ((h >> 16) % 65) as u8,
                a,
            ]
            .as_slice(),
        );
    }
    out
}

/// `None` where the card would not hold the atlas.
///
/// The `--budget` flag is a *self-imposed* bound and the card's is the real one,
/// so the two disagree and this is the half that matters: 4096² at 54 slots is
/// 3.62 GB, comfortably inside a 4 GB budget and refused by a 10 GB card that is
/// also running the artist's own Umber and this example's other stores. Without
/// this the run dies in wgpu's uncaptured-error handler and every cell is lost,
/// where the one that could not be built is all that had to be skipped.
///
/// `CanvasRenderer::try_reserve`'s shape, including the part that is easy to get
/// wrong: the scope is popped **before any view is built**, because a view of a
/// failed texture is `CreateTextureViewError::InvalidResource`, which classifies
/// as *Validation* — a filter an `OutOfMemory` scope does not catch, so it would
/// reach the uncaptured handler one line after the check.
#[allow(clippy::too_many_arguments)]
fn build_store(
    gpu: &Gpu,
    grid: &Grid,
    slots: u32,
    residency: Residency,
    label: &str,
) -> Option<Store> {
    let page = grid.page_size();
    let per_page = grid.tiles_per_page();

    // Which tiles each slot backs, and where each lands in the atlas.
    let mut entries: Vec<Vec<Entry>> = Vec::with_capacity(slots as usize);
    let mut cursor = 0u32;
    let mut backed = 0u64;
    for slot in 0..slots {
        let mask = backed_tiles(grid, slot, residency);
        let mut slot_entries = Vec::with_capacity(mask.len());
        for (i, &live) in mask.iter().enumerate() {
            if !live {
                slot_entries.push(Entry::UNBACKED);
                continue;
            }
            backed += 1;
            let (page_index, cell) = match residency {
                // Identity: slot `s` at page `s`, tile (tx, ty) at cell
                // (tx, ty). This is what makes the sampled substitution exact.
                Residency::Dense => (slot, i as u32),
                // Packed: tiles compacted into the pages they need, which is
                // the production layout.
                _ => {
                    let at = cursor;
                    cursor += 1;
                    (at / per_page, at % per_page)
                }
            };
            slot_entries.push(Entry::at(
                page_index,
                cell % grid.tiles.x,
                cell / grid.tiles.x,
            ));
        }
        entries.push(slot_entries);
    }
    let pages = match residency {
        Residency::Dense => slots,
        _ => cursor.div_ceil(per_page).max(1),
    };

    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let atlas = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: page.x,
            height: page.y,
            depth_or_array_layers: pages,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: LAYER_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[LAYER_FORMAT_LINEAR],
    });
    // Popped here, with no view yet built. See this function's docs. `block_on`
    // over a future wgpu builds with `ready(...)`: an extractor, not a wait.
    if let Some(error) = pollster::block_on(scope.pop()) {
        // Dropped, never used: every view of it would be a validation error.
        drop(atlas);
        println!(
            "  {:<8} REFUSED by the card at {} page(s) = {}: {error}",
            residency.label(),
            pages,
            gigabytes(u64::from(pages) * u64::from(page.x) * u64::from(page.y) * 4),
        );
        return None;
    }
    // **The upload bands and waits, and that is not tidiness.** A whole page of
    // 4096² is 67 MB of staging, `write_texture` asks the HAL for it with no
    // `max_buffer_size` check, and a submit does not release staging — it hands
    // it to that submission's fence. So fifty-four pages accumulate 3.6 GB, and
    // when that fails `handle_hal_error` calls `lose`: **no error scope catches
    // it and the device is gone.** The scope above covers the allocation and
    // nothing else, which is exactly the split CLAUDE.md records for
    // `write_layer_rect`. Waiting per band is what makes the staging alive at
    // any instant one band, and this is setup rather than a frame.
    let fill = page_bytes(page, grid.doc_size);
    let row_bytes = u64::from(page.x) * 4;
    let band = (UPLOAD_BAND_BYTES / row_bytes.max(1)).clamp(1, u64::from(page.y)) as u32;
    for slice in 0..pages {
        let mut y = 0;
        while y < page.y {
            let rows = band.min(page.y - y);
            gpu.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &atlas,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: 0, y, z: slice },
                    aspect: wgpu::TextureAspect::All,
                },
                &fill[(u64::from(y) * row_bytes) as usize..],
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(page.x * 4),
                    rows_per_image: Some(rows),
                },
                wgpu::Extent3d {
                    width: page.x,
                    height: rows,
                    depth_or_array_layers: 1,
                },
            );
            gpu.queue.submit([]);
            let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
            y += rows;
        }
    }

    let table = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("page-table"),
        size: wgpu::Extent3d {
            width: grid.tiles.x,
            height: grid.tiles.y,
            depth_or_array_layers: slots,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: PAGE_TABLE_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (slot, slot_entries) in entries.iter().enumerate() {
        let raw: Vec<u32> = slot_entries.iter().map(|e| e.0).collect();
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &table,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: slot as u32,
                },
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&raw),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(grid.tiles.x * 4),
                rows_per_image: Some(grid.tiles.y),
            },
            wgpu::Extent3d {
                width: grid.tiles.x,
                height: grid.tiles.y,
                depth_or_array_layers: 1,
            },
        );
    }

    Some(Store {
        atlas_view: atlas.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        }),
        raw_view: atlas.create_view(&wgpu::TextureViewDescriptor {
            format: Some(LAYER_FORMAT_LINEAR),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            usage: Some(wgpu::TextureUsages::TEXTURE_BINDING),
            ..Default::default()
        }),
        table_view: table.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        }),
        pages,
        backed,
        bytes: u64::from(pages) * u64::from(page.x) * u64::from(page.y) * 4,
    })
}

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------

fn submit_batch(
    gpu: &Gpu,
    pipeline: &wgpu::RenderPipeline,
    bind_group: &wgpu::BindGroup,
    target: &wgpu::TextureView,
    passes: u32,
) {
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    for _ in 0..passes {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("measure-composite"),
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
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
    gpu.queue.submit([encoder.finish()]);
}

/// Milliseconds per composite pass, one figure per sample.
fn time_cell(
    gpu: &Gpu,
    pipeline: &wgpu::RenderPipeline,
    bind_group: &wgpu::BindGroup,
    target: &wgpu::TextureView,
    plan: &Plan,
) -> Vec<f64> {
    for _ in 0..plan.warmup {
        submit_batch(gpu, pipeline, bind_group, target, plan.passes);
        let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
    }
    let mut out = Vec::with_capacity(plan.samples);
    for _ in 0..plan.samples {
        let started = Instant::now();
        submit_batch(gpu, pipeline, bind_group, target, plan.passes);
        let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
        out.push(started.elapsed().as_secs_f64() * 1000.0 / f64::from(plan.passes));
    }
    out
}

/// The submit and the fence with nothing in the encoder: the floor this
/// instrument can resolve. Any figure near it is a figure to refuse to quote.
fn noise_floor(gpu: &Gpu, plan: &Plan) -> Vec<f64> {
    let mut out = Vec::with_capacity(plan.samples);
    for _ in 0..plan.samples + plan.warmup {
        let started = Instant::now();
        let encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        gpu.queue.submit([encoder.finish()]);
        let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
        out.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    out.drain(..plan.warmup);
    out
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let at = (q * (sorted.len() - 1) as f64).round() as usize;
    sorted[at.min(sorted.len() - 1)]
}

struct Summary {
    median: f64,
    low: f64,
    high: f64,
}

fn summarise(mut v: Vec<f64>) -> Summary {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    Summary {
        median: quantile(&v, 0.5),
        low: quantile(&v, 0.1),
        high: quantile(&v, 0.9),
    }
}

// ---------------------------------------------------------------------------
// The sweep
// ---------------------------------------------------------------------------

struct Plan {
    sizes: Vec<UVec2>,
    layers: Vec<u32>,
    zooms: Vec<Zoom>,
    output: UVec2,
    budget: u64,
    passes: u32,
    samples: usize,
    warmup: usize,
    repeat: usize,
    masks: bool,
    choice: Choice,
}

#[derive(Clone, Copy, PartialEq)]
enum Zoom {
    Fit,
    At(f32),
}

impl Zoom {
    fn label(self) -> String {
        match self {
            Zoom::Fit => "fit".into(),
            Zoom::At(z) => format!("{z}"),
        }
    }
    fn resolve(self, doc: UVec2, view: UVec2) -> f32 {
        match self {
            Zoom::Fit => (view.x as f32 / doc.x as f32).min(view.y as f32 / doc.y as f32),
            Zoom::At(z) => z,
        }
    }
}

fn parse_size(s: &str) -> Option<UVec2> {
    let (w, h) = s.split_once(['x', 'X'])?;
    Some(UVec2::new(w.trim().parse().ok()?, h.trim().parse().ok()?))
}

fn gigabytes(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.2} GB", bytes as f64 / (1u64 << 30) as f64)
    } else {
        format!("{:.0} MB", bytes as f64 / (1u64 << 20) as f64)
    }
}

fn plan_from_args() -> Plan {
    let mut plan = Plan {
        sizes: vec![UVec2::new(1920, 1080), UVec2::new(2048, 2048)],
        layers: vec![1, 8, 16, 32, 54],
        zooms: vec![Zoom::Fit, Zoom::At(1.0)],
        output: UVec2::new(1920, 1080),
        // The dense atlas is what bounds this: it is one canvas-sized page per
        // slot, which is exactly the store the atlas replaced. Anything past
        // the budget is skipped and said out loud.
        budget: 4 << 30,
        // Several passes per submit so the fence is amortised rather than
        // counted once per pass: at one pass each the empty-submit floor is a
        // large fraction of a small cell and the A/B would be measuring the
        // driver.
        passes: 32,
        samples: 25,
        warmup: 5,
        repeat: 1,
        masks: false,
        choice: Choice::Best,
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].clone();
        // The value of a flag that takes one. Empty where the flag was last on
        // the line, which every parse below then falls back from.
        let value = args.get(i + 1).cloned().unwrap_or_default();
        let mut ate = true;
        match flag.as_str() {
            "--fallback" => {
                plan.choice = Choice::Fallback;
                ate = false;
            }
            "--masks" => {
                plan.masks = true;
                ate = false;
            }
            "--sizes" => plan.sizes = value.split(',').filter_map(parse_size).collect(),
            "--layers" => {
                plan.layers = value
                    .split(',')
                    .filter_map(|s| s.trim().parse().ok())
                    .collect();
            }
            "--zooms" => {
                plan.zooms = value
                    .split(',')
                    .filter_map(|s| match s.trim() {
                        "fit" => Some(Zoom::Fit),
                        n => n.parse().ok().map(Zoom::At),
                    })
                    .collect();
            }
            "--output" => plan.output = parse_size(&value).unwrap_or(plan.output),
            "--budget" => {
                plan.budget = (value.parse::<f64>().unwrap_or(4.0) * (1u64 << 30) as f64) as u64;
            }
            "--passes" => plan.passes = value.parse().unwrap_or(8).max(1),
            "--samples" => plan.samples = value.parse().unwrap_or(25).max(1),
            "--warmup" => plan.warmup = value.parse().unwrap_or(5),
            "--repeat" => plan.repeat = value.parse().unwrap_or(1).max(1),
            other => {
                eprintln!("ignoring {other}");
                ate = false;
            }
        }
        i += 1 + usize::from(ate);
    }
    assert!(!plan.sizes.is_empty(), "--sizes parsed to nothing");
    assert!(!plan.layers.is_empty(), "--layers parsed to nothing");
    assert!(!plan.zooms.is_empty(), "--zooms parsed to nothing");
    plan
}

fn main() {
    let plan = plan_from_args();
    let gpu = pollster::block_on(Gpu::with_adapter(Gpu::create_instance(), None, plan.choice))
        .unwrap_or_else(|e| panic!("{e}"));
    let info = gpu.adapter.get_info();
    println!(
        "adapter: {} ({:?}, {:?})",
        info.name, info.device_type, info.backend
    );
    println!(
        "output {} x {}, {} pass(es) per submit, {} sample(s) after {} warm-up",
        plan.output.x, plan.output.y, plan.passes, plan.samples, plan.warmup
    );

    let floor = summarise(noise_floor(&gpu, &plan));
    println!(
        "noise floor: an empty submit and fence is {:.3} ms [{:.3}, {:.3}], which \
         over {} passes is {:.4} ms per pass.",
        floor.median,
        floor.low,
        floor.high,
        plan.passes,
        floor.median / f64::from(plan.passes),
    );
    println!(
        "Anything within a few times that is a figure this instrument cannot resolve. \
         The sampled column is also timed once per residency row on identical work, so \
         its spread down a block of three is a second, independent reading of the noise."
    );

    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("measure-target"),
        size: wgpu::Extent3d {
            width: plan.output.x,
            height: plan.output.y,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TARGET_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&Default::default());

    for round in 0..plan.repeat {
        if plan.repeat > 1 {
            println!(
                "\n================ round {} of {} ================",
                round + 1,
                plan.repeat
            );
        }
        for &size in &plan.sizes {
            sweep_canvas(&gpu, &plan, size, &target, &target_view);
        }
    }
}

fn sweep_canvas(
    gpu: &Gpu,
    plan: &Plan,
    doc: UVec2,
    target: &wgpu::Texture,
    target_view: &wgpu::TextureView,
) {
    let grid = Grid::new(doc);
    let page = grid.page_size();
    let slots = plan.layers.iter().copied().max().unwrap_or(1);
    let dense_bytes = u64::from(slots) * u64::from(page.x) * u64::from(page.y) * 4;

    println!(
        "\n---- canvas {} x {} (page {} x {}, {} tiles, {} slot(s)) ----",
        doc.x,
        doc.y,
        page.x,
        page.y,
        grid.tiles_per_page(),
        slots
    );
    if dense_bytes > plan.budget {
        println!(
            "SKIPPED: the dense atlas the baseline needs is {} against a budget of {}. \
             Raise it with --budget <GB> if the card has the room.",
            gigabytes(dense_bytes),
            gigabytes(plan.budget)
        );
        return;
    }

    // ---- the stores ----------------------------------------------------
    //
    // A refused store takes the whole canvas out rather than only its own rows:
    // the sampled baseline always reads the dense one, so without it there is
    // nothing for the other two to be compared against.
    let stores: Vec<(Residency, Store)> = Residency::ALL
        .iter()
        .filter_map(|&r| build_store(gpu, &grid, slots, r, "atlas").map(|s| (r, s)))
        .collect();
    if stores.len() != Residency::ALL.len() {
        println!(
            "SKIPPED: the card would not hold every store this canvas needs. \
             Ask for fewer layers with --layers, or a smaller canvas."
        );
        return;
    }
    for (r, s) in &stores {
        println!(
            "  {:<8} {} page(s) = {:>8}, {} tile(s) backed of {} ({:.1}%)",
            r.label(),
            s.pages,
            gigabytes(s.bytes),
            s.backed,
            u64::from(slots) * u64::from(grid.tiles_per_page()),
            s.backed as f64 / (u64::from(slots) * u64::from(grid.tiles_per_page())) as f64 * 100.0,
        );
    }

    // ---- the rest of the bindings --------------------------------------
    let stroke = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("scratch"),
        size: wgpu::Extent3d {
            width: doc.x,
            height: doc.y,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: STROKE_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let stroke_view = stroke.create_view(&Default::default());
    let stroke_color = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("stroke-color"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let stroke_color_view = stroke_color.create_view(&Default::default());
    let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("measure-sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });
    let uniforms = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("view"),
        size: std::mem::size_of::<ViewUniforms>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let layout = gpu
        .device
        .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("measure-bgl"),
            entries: &[
                entry_uniform(0),
                entry_texture_array(1),
                entry_texture(2),
                entry_sampler(3),
                entry_texture(4),
                entry_page_table(5),
                // `mask_tex` — the raw view of the atlas. See `Store::raw_view`.
                entry_texture_array(6),
                // The extra sampler every variant declares. See EXTRA_BINDINGS.
                entry_sampler(7),
            ],
        });
    let pipeline_layout = gpu
        .device
        .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("measure-pl"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });

    let pipelines: Vec<(Variant, wgpu::RenderPipeline)> = Variant::ALL
        .iter()
        .map(|&v| {
            let module = gpu
                .device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some(v.label()),
                    source: wgpu::ShaderSource::Wgsl(shader_source(v, page).into()),
                });
            let pipeline = gpu
                .device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(v.label()),
                    layout: Some(&pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &module,
                        entry_point: Some("vs"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &module,
                        entry_point: Some("fs"),
                        compilation_options: Default::default(),
                        targets: &[Some(TARGET_FORMAT.into())],
                    }),
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    multiview_mask: None,
                    cache: None,
                });
            (v, pipeline)
        })
        .collect();

    let bind_groups: Vec<(Residency, wgpu::BindGroup)> = stores
        .iter()
        .map(|(r, s)| {
            let bg = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(r.label()),
                layout: &layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: uniforms.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&s.atlas_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&stroke_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(&stroke_color_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::TextureView(&s.table_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 6,
                        resource: wgpu::BindingResource::TextureView(&s.raw_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 7,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            });
            (*r, bg)
        })
        .collect();

    let find_pipeline = |v: Variant| &pipelines.iter().find(|(k, _)| *k == v).unwrap().1;
    let find_bg = |r: Residency| &bind_groups.iter().find(|(k, _)| *k == r).unwrap().1;

    // ---- the check the whole table rests on ----------------------------
    //
    // Two renderings of one picture. A hardware bilinear tap and a hand lerp of
    // four `textureLoad`s agree to within the last bit rather than exactly, so
    // this reports the largest deviation rather than asserting equality — but a
    // large one means the A/B is comparing two different pictures.
    {
        // Offset by a fraction of a pixel deliberately. `tiles.wgsl` records
        // that a tap landing on a texel centre comes out with weights of
        // exactly 0 and 1 and returns exactly one stored texel — so a check at
        // a whole offset would compare two point samples and agree exactly
        // while saying nothing whatever about the lerp. This puts a real weight
        // on all four taps, which is the thing that could disagree.
        let camera = Camera {
            center: Vec2::new(doc.x as f32 / 2.0 + 0.371, doc.y as f32 / 2.0 + 0.629),
            zoom: 1.0,
        };
        let pivot = Vec2::new(plan.output.x as f32 / 2.0, plan.output.y as f32 / 2.0);
        gpu.queue.write_buffer(
            &uniforms,
            0,
            bytemuck::bytes_of(&view_uniforms(doc, &camera, pivot, 4.min(slots), false)),
        );
        // Every residency, not only the dense one. The dense store is the only
        // one the *sampled* baseline can be compared against — it is the only
        // identity layout — but `gather` has to answer for the packed atlas
        // too, and for the unbacked branch, which the dense store never takes.
        for &residency in &Residency::ALL {
            let bg = find_bg(residency);
            let a = read_back(
                gpu,
                find_pipeline(Variant::Tiled),
                bg,
                target,
                target_view,
                plan.output,
            );
            let g = read_back(
                gpu,
                find_pipeline(Variant::Gather),
                bg,
                target,
                target_view,
                plan.output,
            );
            let worst = worst_deviation(&a, &g);
            // **Exact, not close.** `gather` is a pure optimisation of a path
            // with a known-correct answer: it reads the same stored texels and
            // runs the same f32 lerp, so anything other than zero means it is
            // reading different texels — a shifted picture, a wrong component
            // order, or the canvas-edge clamp not being reproduced.
            println!(
                "  check: gather against tiled on the {} store, 4 layers at 1:1 — \
                 largest channel deviation {worst} of 255{}",
                residency.label(),
                if worst == 0 {
                    "  (exact)"
                } else {
                    "  <-- NOT EXACT. gather is reading different texels; the column is void."
                }
            );

            if residency != Residency::Dense {
                continue;
            }
            let b = read_back(
                gpu,
                find_pipeline(Variant::Sampled),
                bg,
                target,
                target_view,
                plan.output,
            );
            let worst = worst_deviation(&a, &b);
            println!(
                "  check: tiled against sampled on the dense store, 4 layers at 1:1 — \
                 largest channel deviation {worst} of 255{}",
                if worst <= 2 {
                    ""
                } else {
                    "  <-- LARGE. The two columns may not be one picture; treat the table as void."
                }
            );
        }
    }

    // ---- the sweep ------------------------------------------------------
    println!();
    println!(
        "  {:<6} {:<5} {:<8} {:>11} {:>11} {:>11} {:>11} {:>8} {:>8}",
        "zoom",
        "lyrs",
        "residency",
        "tiled ms",
        "gather ms",
        "sampled ms",
        "table ms",
        "tiled/s",
        "gath/s"
    );
    for &zoom in &plan.zooms {
        for &layers in &plan.layers {
            if layers > slots {
                continue;
            }
            let z = zoom.resolve(doc, plan.output);
            let camera = Camera {
                center: Vec2::new(doc.x as f32 / 2.0, doc.y as f32 / 2.0),
                zoom: z,
            };
            let pivot = Vec2::new(plan.output.x as f32 / 2.0, plan.output.y as f32 / 2.0);
            for &residency in &Residency::ALL {
                gpu.queue.write_buffer(
                    &uniforms,
                    0,
                    bytemuck::bytes_of(&view_uniforms(doc, &camera, pivot, layers, plan.masks)),
                );
                let bg = find_bg(residency);
                let tiled = summarise(time_cell(
                    gpu,
                    find_pipeline(Variant::Tiled),
                    bg,
                    target_view,
                    plan,
                ));
                // The baseline is always the dense store: a dense slice has no
                // holes to skip, which is exactly the point of the comparison.
                let sampled = summarise(time_cell(
                    gpu,
                    find_pipeline(Variant::Sampled),
                    find_bg(Residency::Dense),
                    target_view,
                    plan,
                ));
                let table = summarise(time_cell(
                    gpu,
                    find_pipeline(Variant::Table),
                    bg,
                    target_view,
                    plan,
                ));
                let gather = summarise(time_cell(
                    gpu,
                    find_pipeline(Variant::Gather),
                    bg,
                    target_view,
                    plan,
                ));
                println!(
                    "  {:<6} {:<5} {:<8} {:>7.3} ±{:<3.0} {:>7.3} ±{:<3.0} {:>7.3} ±{:<3.0} \
                     {:>7.3} ±{:<3.0} {:>7.2}x {:>7.2}x",
                    zoom.label(),
                    layers,
                    residency.label(),
                    tiled.median,
                    spread_pct(&tiled),
                    gather.median,
                    spread_pct(&gather),
                    sampled.median,
                    spread_pct(&sampled),
                    table.median,
                    spread_pct(&table),
                    tiled.median / sampled.median,
                    gather.median / sampled.median,
                );
            }
        }
    }
}

/// The 10-90 spread as a percentage of the median: what says whether the
/// machine was quiet enough for the figure beside it to mean anything.
fn spread_pct(s: &Summary) -> f64 {
    (s.high - s.low) / s.median * 100.0
}

/// The largest channel difference between two renderings of one frame.
fn worst_deviation(a: &[u8], b: &[u8]) -> u32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (i32::from(*x) - i32::from(*y)).unsigned_abs())
        .max()
        .unwrap_or(0)
}

fn read_back(
    gpu: &Gpu,
    pipeline: &wgpu::RenderPipeline,
    bind_group: &wgpu::BindGroup,
    target: &wgpu::Texture,
    target_view: &wgpu::TextureView,
    size: UVec2,
) -> Vec<u8> {
    let row = (size.x * 4).div_ceil(256) * 256;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(row) * u64::from(size.y),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    submit_batch(gpu, pipeline, bind_group, target_view, 1);
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: target,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(size.y),
            },
        },
        wgpu::Extent3d {
            width: size.x,
            height: size.y,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
    let view = buffer.slice(..).get_mapped_range();
    let mut out = Vec::with_capacity((size.x * size.y * 4) as usize);
    for y in 0..size.y as usize {
        let start = y * row as usize;
        out.extend_from_slice(&view[start..start + (size.x * 4) as usize]);
    }
    drop(view);
    buffer.unmap();
    out
}

// ---------------------------------------------------------------------------
// Bind group layout entries, matching `canvas.rs`'s helpers exactly
// ---------------------------------------------------------------------------

fn entry_uniform(binding: u32) -> wgpu::BindGroupLayoutEntry {
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

fn entry_texture(binding: u32) -> wgpu::BindGroupLayoutEntry {
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

fn entry_texture_array(binding: u32) -> wgpu::BindGroupLayoutEntry {
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

fn entry_page_table(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Uint,
            view_dimension: wgpu::TextureViewDimension::D2Array,
            multisampled: false,
        },
        count: None,
    }
}

fn entry_sampler(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

/// `TILE` is read from `umber_core` rather than restated, so this cannot drift
/// from what the shader compiles. Named here because nothing else in this file
/// uses it and an unused import would be a warning.
const _: () = assert!(TILE == 256);
