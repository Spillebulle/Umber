# Brushes

How Umber's brush library is put together, what an imported brush keeps, and
what it loses.

## The library, in three parts

| Part | Where | Written by |
|---|---|---|
| Umber's own five presets | `umber_defaults` in `crates/umber-core/src/preset.rs` | by hand |
| The imported set | `crates/umber-core/assets/builtin-brushes.ron`, embedded with `include_str!` | the generator, below |
| The user's own | `%APPDATA%\Umber\data\brushes.ron` (Windows), `~/.local/share/umber/brushes.ron` (Linux), `~/Library/Application Support/Umber/brushes.ron` (macOS) | the app, at runtime |

The first two together are `umber_core::preset::builtin()`, a `&'static
[BrushPreset]` parsed once. The third is `umber_core::preset::UserLibrary`.

They are kept apart on purpose. The shipped library is replaced wholesale by an
update; anything the user saved into it would be lost. The user library is never
written by the build.

## Refreshing the shipped library

```sh
pwsh tools/fetch-brushes.ps1     # or: sh tools/fetch-brushes.sh
cargo run -p umber-core --example build-brush-library
```

The first step downloads the packs into `assets/brushes/`, which is git-ignored,
and records what it took in `assets/brushes/LICENSES.md`, which is not. The
second converts them and rewrites `builtin-brushes.ron`. Both steps are
deliberate acts rather than a `build.rs`: the packs are not in the repository,
so a build script would make a clean checkout unbuildable, and a generated file
that lands in a commit is a file whose diff can be read.

Preview thumbnails are never downloaded. In the MyPaint pack the brush
*settings* are CC0 but some of the previews are CC-BY, and not having the files
is the surest way not to ship them.

## What is in it today

128 MyPaint brushes plus Umber's own five, all CC0. 68 of the pack's 196 were
refused by the generator, not lost by accident:

| Refused | Count | Why |
|---|---|---|
| `smudge >= 0.5` | 67 | Umber's dab pass writes coverage into a scratch texture and never reads the layer, so a brush cannot pick colour up off the canvas. Imported, a blender would paint solid colour under a name that promises the opposite. |
| time-based dabs only | 2 | `dabs_per_second` with no distance term. Umber's dab loop is driven by distance travelled, so the brush would import as a solid line. |

(One brush is refused on both counts, hence 68 rather than 69.)

`umber_core::brushimport::mypaint::unsupported_features` is the check, and it is
deliberately separate from the importer: a user who asks for a specific file
should get whatever the importer can make of it. The generator is the fussy one,
because it decides what Umber *claims* to support.

## What conversion keeps

`.myb` is JSON. MyPaint evaluates each setting as

```text
value = base_value + Σ mapping_i(input_i)
```

— the input mappings are **added** to the base value, not multiplied by it.

| MyPaint | Umber |
|---|---|
| `radius_logarithmic` | `size`, `min_size_ratio`, `size_curve` |
| `hardness` | `hardness` |
| `opaque` × `opaque_multiply` | `opacity`, `opacity_curve`, `pressure_opacity` |
| `dabs_per_actual_radius` + `dabs_per_basic_radius` | `spacing` |
| `eraser` | `mode` |
| `slow_tracking` | `stabilization` |

`radius_logarithmic` is the natural log of the dab radius **in pixels**, and its
pressure mapping is an offset in log space, so the radius at pressure *p* is
`exp(base + map(p))`. The classic mis-import is to read the base value as a
radius: it turns a 2.6 px pen into a 0.96 px one. `classic/pen` has a base of
0.96 and a pressure mapping to +0.5, so Umber stores a size of 8.61 px, a
minimum ratio of `exp(-0.5) = 0.607`, and a size curve that reproduces
MyPaint's radius exactly at all five sample points. There is a test for that:
`the_imported_radius_matches_mypaint_at_every_sample`.

## What conversion loses

Documented in full in the module docs of
`crates/umber-core/src/brushimport/mypaint.rs`. The short version, worst first:

- **Elliptical dabs.** `elliptical_dab_ratio` and `elliptical_dab_angle` have no
  equivalent — Umber's dab is a circle. Around a quarter of the MyPaint set is
  elliptical, and those brushes import as round ones with no line-weight
  variation. This is the single biggest loss.
- **Scatter and jitter.** `offset_by_random`, `radius_by_random`,
  `offset_by_speed`. Spray, splatter and "bulk" brushes come out as smooth lines.
- **Bitmap tips.** MyPaint has none either — a `.myb` is always a round dab, so
  nothing is lost here. The engine now has them (see below); it is the Krita
  and GIMP packs they exist for.
- **Non-pressure inputs.** `speed1`, `speed2`, `random`, `stroke`, `direction`,
  `tilt`. `Brush` has exactly two pressure-driven parameters and nowhere to put
  the rest.
- **Opacity build-up.** MyPaint composites each dab, so a low-opacity brush
  darkens as a stroke crosses itself. Umber takes a `max` of coverage across the
  whole stroke and applies opacity once at commit — that is the wet-layer design
  in `CLAUDE.md`, and it is why `opaque_linearize` is ignored rather than
  approximated. A MyPaint wash and an Umber wash of the same numbers will not
  look the same.

## Bitmap tips

The dab pass can stamp an 8-bit coverage mask instead of its procedural round
falloff. This is what the stamp-based packs need.

`umber_core::tip::TipMask` is the mask — plain bytes, `0` no paint and `255`
full, so the engine keeps no GPU types. `CanvasRenderer::set_tip` uploads one to
an `R8Unorm` texture and flips a flag in the dab uniforms.

Three things about the design are load-bearing:

- **The tip is bound per pass, not per dab.** A stroke has one brush, so one
  tip covers the whole dab pass and a thousand tipped dabs are still a single
  draw call. Set it between strokes; changing it mid-stroke would restamp what
  is already in the scratch under a new shape.
- **The tip modulates coverage; it does not composite.** The blend state is
  untouched and still `max`, so a tipped stroke saturates at 1.0 under overlap
  exactly as a round one does and stroke opacity is still applied once, at
  commit. `a_tipped_stamp_still_saturates_under_overlap` is the guard, and it is
  a deliberate copy of `overlapping_dabs_do_not_compound` rather than an
  extension of it.
- **The round path is untouched.** The shader samples the tip unconditionally —
  `textureSample` may not sit in non-uniform control flow — and then `select`s
  between it and the falloff. With no tip the binding is a 1×1 placeholder whose
  contents are discarded. Every pre-existing GPU test still passes unchanged,
  which is the evidence.

`umber_core::brushimport::gbr::from_gbr` decodes a GIMP `.gbr` into a
`TipMask`, plus the brush name and the spacing the format carries. Everything in
it is big-endian; a little-endian read reports a billion-pixel brush and is
caught by the length check rather than producing garbage.

## Not done yet

- **Somewhere to keep a tip.** The preset library is a text file and a tip is a
  bitmap, so a `BrushPreset` cannot yet name one — which is why `.gbr` is
  absent from `brushimport::read_file`. The library needs to become a directory
  with the RON alongside a `tips/` folder before a stamp brush can be *saved*,
  as opposed to loaded and used. Until then a caller decodes a `.gbr` itself and
  hands the mask to `set_tip`.
- **A licensed `.gbr` pack.** None of the candidate sources states its licence
  inside the download, so none is fetched — see `docs/brush-sources.md`. The
  `.gbr` decoder is tested against files built byte by byte in the test module,
  not against a real brush.
- **Elliptical tips.** The tip is stretched over the dab's bounding square, so a
  non-square mask loses its aspect ratio. The dab carries a single radius and
  has nowhere to record one.
- **Grain / paper texture** multiplied into dab coverage.
- **Elliptical and rotating dabs**, which would recover a quarter of the MyPaint
  set properly rather than approximately.
- **A `.kpp` importer.** Krita presets are PNG files with the settings in a
  text chunk, and most of them lean on a bitmap tip, so they need the tip work
  first.

See `docs/brush-sources.md` for the packs that were considered and why the ones
that are missing are missing.
