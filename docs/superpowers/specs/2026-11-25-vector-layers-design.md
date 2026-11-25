# Vector layers, stage 1: the layer itself

Date: 2026-11-25. Status: awaiting approval.

## Intent

The artist wants to cut stickers on a Silhouette Portrait 3 from what was drawn
in Umber, and later to plot. That needs lines a machine can follow: drawn by
hand, generated from the artwork, and easy to edit afterwards. The agreed order
is four projects, brainstormed one at a time:

1. **Vector layers** (this document): a layer that holds paths, saves, reopens,
   undoes and displays.
2. Drawing and editing paths: freehand, point by point, node editing.
3. Generating paths: a cut contour traced from artwork, with an offset.
4. The machine: printing with registration marks, sending to the Portrait 3.

The artist also wants **full vector layers** eventually, meaning brush-look
strokes that stay editable (Clip Studio's vector layers). That is not built
here. What is built here must not have to be redone when it arrives.

## Goals

- A vector layer exists in the stack beside raster layers, folders and text
  layers, and behaves like a layer: visible, lockable, reorderable, in folders,
  deletable, all undoable.
- It holds paths. Each path has a role: **Cut**, **Plot**, or (reserved) **Ink**.
- Cut and Plot paths draw over the canvas as crisp lines at any zoom, never
  enter the layer's pixels, and therefore never appear in an export, the
  flattened preview, the eyedropper or a smudge pickup.
- The record round-trips through Save, autosave and Open, byte-stable.
- A path change is one undo entry.
- One real way to make a path, so the feature is visible and useful on its own:
  **a path from the current selection's outline.**
- The file format and the model carry everything Ink paths will need, so adding
  them later changes no document already saved.

## Non-goals

- Drawing tools, node editing, deleting or moving a single path (project 2).
- Tracing a contour from artwork, offsetting (project 3).
- Anything about printers, registration marks or the Silhouette (project 4).
- Rendering Ink paths through the brush engine.
- Importing Clip Studio or Krita vector layers. They stay refused with the
  existing sentence.
- SVG import or export.

## Anti-goals

- A cut line that shows up in the printed picture.
- A second kind of "layer" outside the stack that masks, folders and undo do not
  understand.
- A control that is drawn and does nothing. Ink has no control anywhere in this
  stage.
- An older Umber opening a document, saving it, and silently deleting every cut
  line.

## Approaches considered

**A. A record beside an ordinary layer** (what `textobj` does). The layer keeps
its slot; paths are a record on `Layer`, written under `umber/vector/`. Brush
strokes are later re-rendered into the layer's own pixels. Composite, masks,
effects, thumbnails and export are untouched. Cost: baked pixels are not crisp
when zoomed and a baked cut line would print.

**B. A slotless layer drawn as an overlay**, like the marquee. Crisp, free, never
exports. But it is outside the picture, so it can never be artwork, and Ink
would need a second system.

**C. Curves rendered inside `composite.wgsl`.** Crisp and in the stack, and it
rewrites the single pass everything reuses.

**Chosen: A, with B's drawing for machine lines.** One layer kind. Cut and Plot
paths are overlay lines and never rasterise. Ink paths, later, rasterise into
the layer's slot through the real dab pipeline. The artist picked this.

## Design

### The model: `umber-core::vector`

A new module, no GPU or egui types in it.

```rust
pub struct VectorObject { paths: Vec<Path> }

pub struct Path {
    pub role: PathRole,
    pub closed: bool,
    pub nodes: Vec<Node>,
    pub ink: Option<Ink>,      // reserved; None for Cut and Plot
}

pub struct Node {
    pub at: Vec2,              // document pixels
    pub handle_in: Vec2,       // offset from `at`; zero is a corner
    pub handle_out: Vec2,
    pub pressure: f32,         // reserved for Ink; 1.0
}

pub enum PathRole { Cut, Plot, Ink }

pub struct Ink {               // reserved
    pub brush: String,         // a preset's stable id
    pub size: f32,
    pub colour: [f32; 4],      // through effect::linear_rgba's shape
}
```

- A path is a chain of cubic segments between consecutive nodes: `at`,
  `at + handle_out`, `next.at + next.handle_in`, `next.at`. Zero handles make a
  straight segment. Document pixels, y-down. Physical size comes from
  `Document::dpi` at send time and is not stored on the path.
- `Path::flatten(tolerance, &mut Vec<Vec2>)` turns a path into a polyline within
  `tolerance` document pixels. It is the one flattener: the overlay uses it now,
  and hit testing, tracing output and the machine will later. Straight segments
  emit their endpoints only.
- Positions and handles must be finite. A path needs at least two nodes, or it
  is dropped on the way in.
- `VectorObject::flipped(axis, doc)` mirrors every node and handle. Exact for
  figures that are binary fractions; for any other, a double flip moves a figure
  by at most one rounding at the canvas's magnitude and repeated flips do not
  accumulate (tested over a thousand round trips). Amended after review: the
  first draft said "exact", which is false in `f32`.
- `VectorObject::translated(offset)` for a canvas resize.
- `VectorObject::from_rings(rings, role)` builds closed, all-corner paths from a
  selection's rings. It merges exactly collinear runs and nothing more, so a
  rectangle is four nodes. **Stated limit of this stage (my call):** a selection
  that has been through Add, Subtract or Intersect has rings traced back out of
  its mask, so its path is a pixel staircase, and a self-crossing lasso gives a
  path that crosses itself. Smoothing and simplification are project 3's, where
  tracing artwork needs them anyway. The CHANGELOG says so.
- `MAX_NODES` bounds the nodes in one layer (proposed 65,536) and `MAX_PATHS`
  the paths (proposed 4,096). Enforced where a record is installed, so an
  over-long record is unwritable and the file bound below can be derived.
- `byte_len()` for the undo budget, like `TextObject::byte_len`.

Every `match` on `PathRole` is exhaustive, with no `matches!`.

### On the layer

`Layer::vector: Option<Box<VectorObject>>`, private, the invariant owned by
`LayerStack`, exactly as `Layer::text` is.

- A vector layer **holds a slot** like any raster layer. A blank slot costs
  nothing in the tile atlas, masks and effects need no special case, and Ink
  will need the pixels. `LayerStack::MAX` counts it as one entry.
- A layer carries a text record or a vector record, never both. Folders carry
  neither.
- **A layer is a vector layer exactly when it carries a record**, and an empty
  record is a record. A fresh vector layer is `Some(empty)`, refuses paint, is
  written to the file and makes the file revision 5. (My call.)
- `LayerStack::add_vector_layer(name)` goes through `add_refusal`, like
  `add_named`. Default name "Vector".
- `plan_set_vector` / `can_set_vector` / `set_vector` replace the whole record.
  Gated on: index valid, is a vector layer (or is being made one at creation),
  not locked, within `MAX_NODES` and `MAX_PATHS`. A refusal changes nothing.
- **Painting is refused** at `LayerStack::refusal_at` with a new
  `EditRefusal::Vector`: "This is a vector layer. Paint on an ordinary layer
  instead." A stroke on its **mask** is allowed, for the reason it is on a text
  layer. Lift, paste and cut are refused at the same gates that already refuse
  a locked layer, with that sentence.
- **"Clear layer" is disabled** on a vector layer with a tooltip. On a text layer
  it means "convert to paint"; here there may be no paint, and silently throwing
  away paths is the wrong reading. Deleting the layer is how paths go, and that
  is undoable.
- **Canvas flip** mirrors the record through `flipped`, in the same place the
  text placement is mirrored.
- **Canvas resize** translates Cut and Plot paths by the anchor offset
  (`CanvasCopy::plan`'s). That is exact for a grow and a shrink, because the
  paths are geometry and may lie outside the canvas. Ink paths are dropped on a
  resize with their pixels kept, which is the text record's rule for the same
  reason.
- A delete parks the layer with its record in the `Gone` entry, so undo brings
  both back with nothing written for it.
- Opacity, blend mode, mask, clipping and effects act on the layer's **pixels**.
  The overlay lines answer only to visibility (`effective_visible`). The layer
  row keeps those controls, because Ink will use them.

### Undo

- Adding a vector layer is `EditKind::AddLayer`, structural, as a text layer's
  creation is.
- Changing the record is a new arm, `EditBody::Vector { slot, was: Option<Box<VectorObject>> }`,
  and a new `EditKind::Paths` ("Edit paths"). The slot identifies the layer for
  the reason a `PixelPatch` names one: a slot never changes hands while an entry
  can name it. Undo swaps the record. Its cost to the budget is `byte_len()`.
- It is a whole-record snapshot per edit. At `MAX_NODES` that is a couple of
  megabytes worst case; project 2 can refine to per-path if it bites.
- `EditKind::Paths` gets its arm in `label`, `flip_axis`, `is_structural`,
  `resurrects_pixels`, `panels::edit_icon` and `ALL`.
- **Not written to a saved history.** `SaveHistory::new` skips it explicitly,
  through an exhaustive match, and `docimport::history`'s guard is checked for
  the same. `history::VERSION` does not move. (CLAUDE.md, "Partial
  exhaustiveness", describes exactly how a new bodiless kind gets written as a
  flip otherwise.)

### The file

- A record is `umber/vector/<n>.json`, named by `umber-vector` on the layer's
  element. The mask's and the text record's shape, for their reason.
- Compact `serde_json`, like the text record, so the bytes do not depend on the
  platform. A `version` field of its own, `vector::VERSION = 1`.
- **A record from a newer revision is kept, not dropped** (my call). The layer
  holds it as `VectorObject::Opaque(bytes)`: still a vector layer, still refuses
  paint, draws no lines, cannot be appended to, and is written back byte for
  byte. A warning says the layer's paths need a newer Umber. Dropping it would
  make the layer paintable and the next save would delete a newer build's paths,
  which is the fourth anti-goal. So `VectorObject` is an enum of `Paths` and
  `Opaque`, matched exhaustively.
- **Per-field defaults and no container `#[serde(default)]`**; `role` has no
  default, so a path missing one is refused rather than read as a Cut. The whole
  serialised text of a fixture record is pinned as a literal, field names and
  role spellings included.
- The record has an **optional** `print` field, the text record's `Fingerprint`
  of the layer image, **written only when the record holds an Ink path** (my
  call). This stage never writes one, so the pinned literal has no `print`. On
  load, a present fingerprint that does not match **drops the Ink paths and
  keeps Cut and Plot**, with a warning: Ink describes pixels, Cut and Plot
  describe none and cannot go stale. Reachable in this stage only from a
  hand-built fixture, which is what tests it.
- **`docformat::VERSION` goes to 5**, and `required_version` answers 5 where any
  layer carries a vector record and what it answered before otherwise. Reason:
  an older build that opens and saves drops the paths for ever, and the paths
  are the whole content of a cut layer. That is the argument that took effects
  to 3. Group compositing therefore takes **6**; `docs/group-compositing.md`
  §4.3 is updated to say so.
- Size bound: `MAX_RECORD_BYTES`, derived from `MAX_NODES` and `MAX_PATHS`
  times a measured bytes-per-node, doubled. Checked on the reading side before
  decompression, through `read_optional_entry_bounded`.
- A record that cannot be parsed at all costs that layer its paths and nothing
  else, with a warning, and the layer opens as an ordinary layer. There is
  nothing to preserve.
- **Both writers in one commit.** `SaveLayer::vector` is filled by `app.rs` off
  the layer and by `autosave.rs` off its snapshot (`Candidate` gains the field).
  The guard that counts the `text:` literal at every `SaveLayer` construction
  site gains a twin for `vector:`.
- Other ORA readers ignore the attribute and show a blank layer. That is a
  `SaveWarning`-free loss, since nothing is saved wrong; the export dialogs say
  nothing about it either, because an export was never going to carry a cut
  line.

### On screen

- `ui::vector_paths`, drawn in the canvas overlay beside the marquee, for every
  vector layer whose `effective_visible` is true, clipped to the canvas region.
  Under the transform box and the selection strip.
- Each path is flattened at a tolerance of a quarter of a screen pixel
  (`0.25 / zoom` document pixels), mapped through the camera, and drawn as a
  two-tone line: `Palette::accent_underlay`'s end of the axis underneath, the
  role's colour on top, widths through `ui::ant_width`. Solid, not dashed, so it
  cannot be read as a selection.
- Role colours are palette values, not literals: `Palette::cut_line` and
  `Palette::plot_line`, derived per theme so they are separated from each other,
  from the accent and from the link colours, with a contrast guard that sweeps
  artwork the way `the_marquees_pair_reads_over_any_artwork` does. An Ink path
  read from a newer file draws nothing as an overlay; its pixels are the layer's.
- The flattened screen polylines are cached per layer, keyed on the record's
  `Arc`/revision and the camera, so a still view costs no flattening. A document
  with no vector layer pays one branch.
- The layer row draws a **vector mark** in place of a thumbnail while the layer
  has no Ink (there are no pixels to picture). Lucide `spline`, pasted, with a
  look at `icon_sheet` for collisions.
- The Layers flags-row add control gains "Vector layer" beside the ordinary add,
  wherever the Text placement's own-layer path would put it; exact placement is
  the builder's, following the header-width guard.

### The one way to make a path

Two commands, in `shortcuts` as `Action`s with no default key, on the **Edit
menu** under Deselect (there is no Select menu, and the canvas strip's width is
hand-measured; my call):

- **Cut path from selection**
- **Plot path from selection**

Each takes the live selection's rings, builds closed paths with that role, and
appends them to the active layer if it is an unlocked vector layer, or else
makes a new vector layer above the active one (named "Cut lines" or "Plot
lines") and puts them there. One undo entry either way: `Paths` for an append,
`AddLayer` for a new layer. Disabled, with their key still named, when there is
no selection. A feathered selection gives its sharp rings, which is what the
marquee already shows. The selection is left standing.

A refusal (stack full, `MAX_NODES`) is a notice with a sentence, never a silent
nothing.

## Constraints

- `umber-core` stays free of GPU and egui types; the overlay is `umber-app`'s.
- `composite.wgsl`, `commit.wgsl` and `blend.wgsl` are not touched.
- Nothing on the drawing path allocates per frame.
- User-facing strings: short sentences, British spelling, no em-dashes.
- New `Icon` variants are Lucide pastes.
- Gates: `fmt --check`, `clippy` with `-D warnings`, `test`, and
  `UMBER_TEST_SOFTWARE=1` is not needed since no shader changes.

## Left to the builder

- Exact figures for `MAX_NODES`, `MAX_PATHS` and the derived byte bound, once a
  node's encoded size is measured.
- The flattening algorithm (recursive subdivision or a derived segment count,
  as `ARC_TOLERANCE` does it).
- How the overlay cache is keyed and stored.
- The per-theme values of `cut_line` and `plot_line`, subject to the guards.
- Where exactly "Vector layer" sits among the add controls.
- CHANGELOG, README and CLAUDE.md prose, in one commit at the end.

## Testing

- Model: flatten within tolerance against an analytic curve; a flip twice is the
  identity; `from_rings` round trip; every bound refuses and changes nothing.
- Stack: paint refused, mask stroke allowed; delete and undo restores the
  record; flip mirrors; resize translates; a folder deleted with a vector layer
  in it comes back whole.
- Format: save and reopen is equal; the pinned literal; a newer record revision
  opens as a plain layer; a fingerprint mismatch keeps Cut and drops Ink; an
  over-long entry is refused unread; `required_version` is 5 with a record and
  unchanged without; Save and the autosave write the same record.
- History: an `EditKind::Paths` entry is not written to a saved history and
  does not discard it.
- Interface: the overlay draws a line for a visible vector layer and none for a
  hidden one, measured off the shapes a headless pass produced; the menu rows
  are disabled with no selection; the commands produce one undo entry.
- Export: a document with a cut path exports byte-identically to the same
  document without it.
