# Paper grain

The tiling textures the dab pass multiplies into coverage — what makes a pencil
catch on the tooth of the paper. `Brush::grain` and `Brush::grain_pattern`
choose between them; `crates/umber-core/src/pattern_table.rs` embeds them.

| File | Source | Licence |
|---|---|---|
| `tooth.png` | Umber, `crates/umber-core/examples/build-bitmaps.rs` | CC0-1.0 |
| `canvas.png` | Umber, `crates/umber-core/examples/build-bitmaps.rs` | CC0-1.0 |
| `grit.png` | Umber, `crates/umber-core/examples/build-bitmaps.rs` | CC0-1.0 |

Each is a 256×256 8-bit greyscale PNG. 255 is paper that takes paint freely and
0 is a pit the brush skips entirely.

## Why these are drawn rather than photographed

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

A user's own paper is not a feature yet. `GrainPattern` is an enum rather than a
name because `Brush` is `Copy`; when a user's own texture becomes a feature it
grows a variant that names one, and this table gains rows.
