# Paper grain

The tiling textures the dab pass multiplies into coverage — what makes a pencil
catch on the tooth of the paper. `Brush::grain_pattern` chooses between the
three Umber draws and `BrushPreset::paper` names any of them by file;
`crates/umber-core/src/pattern_table.rs` embeds the lot.

| File | Source | Licence |
|---|---|---|
| `tooth.png` | Umber, `crates/umber-core/examples/build-bitmaps.rs` | CC0-1.0 |
| `canvas.png` | Umber, `crates/umber-core/examples/build-bitmaps.rs` | CC0-1.0 |
| `grit.png` | Umber, `crates/umber-core/examples/build-bitmaps.rs` | CC0-1.0 |
| `deevad-*.png` (4) | David Revoy's Krita bundle, via `build-brush-library` | CC0-1.0 |
| `gdquest-gdquest-texture-fabric.png` | GDquest's Krita presets, via `build-brush-library` | CC-BY-4.0 |

Each is an 8-bit greyscale PNG. 255 is paper that takes paint freely and 0 is a
pit the brush skips entirely.

## Why Umber's own three are drawn rather than photographed

The rule at the top of `docs/brush-sources.md` is that a licence has to be
verifiable **from the files themselves**. The CC0 texture libraries worth having
— ambientCG, Poly Haven, Texture Ninja — all state their licence on a web page
beside the download rather than inside it, which is precisely the case that rule
exists for. Shipping a photograph would mean either claiming a licence that
cannot be checked from the download or hand-waving it, and neither is what this
project does with other people's work.

So they are generated instead. The source is one file, the licence is the
project's own, and both travel with the repository. Regenerate with:

```sh
cargo run -p umber-core --example build-bitmaps
```

Two further things fall out of generating them, and they are not consolation
prizes:

- **They tile by construction.** The grain is anchored to the *document* and
  repeats across it — that is the whole effect, and it is why a second stroke
  lands in the same pits as the first — so a seam would draw a grid over every
  textured mark on the canvas. The noise lattice wraps, so the right edge
  interpolates towards the same lattice points the left edge came from.
  `every_shipped_pattern_tiles_without_a_seam` measures that: the step across
  each seam has to be no larger than the steps inside the tile.
- **Three files, 200 kB.** A photographic set at a resolution worth having would
  be megabytes in a binary that is otherwise a few.

## Why the imported ones may be here at all

The five `deevad-` and `gdquest-` tiles are a *brush pack's* patterns, written
by `crates/umber-core/examples/build-brush-library.rs` when it converts a Krita
preset whose texture Umber can reproduce. They are here on the same terms as the
masks in `crates/umber-core/assets/tips/`, and it is the same decision made
twice: shipping a brush's settings describes somebody's work, and shipping its
bitmap **is** the work. So a pattern travels only from a pack whose licence was
verified inside its own download — Revoy's bundle states CC0 in its `meta.xml`
and GDquest's repository states CC-BY-4.0 in its `README.md`, both of which
`tools/fetch-brushes.*` check — and `Pack::ship_tips` is the one switch for
both. Attribution for the CC-BY one travels on every preset that names it, as
`BrushPreset::credit`, and the browser prints it on the row.

They are **not** untouched copies of the author's file: Krita states a levels
pipeline over the pattern (brightness, contrast, inversion, a cutoff window) and
`brushimport::kpp` bakes it in, so what is stored is the tile that brush paints
through rather than the pattern it started from. Two brushes over one pattern
with different levels are therefore two files, and two with the same levels are
one — deduplication is by content.

Regenerate these with:

```sh
cargo run -p umber-core --example build-brush-library
```

That generator **owns every file here whose name begins with a pack's id
prefix** and deletes the ones a previous run left behind; `tooth`, `canvas` and
`grit` are `build-bitmaps.rs`'s and are left alone.

A paper a *user* draws or imports is a feature and lives in their own library's
`papers/` directory, named from `BrushPreset::paper`. `GrainPattern` stays an
enum over the three above because `Brush` is `Copy`, and the name on the preset
is what overrides it.
