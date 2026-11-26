# Trace outline: cut and plot paths generated from the artwork

Project 3 of four on the way to cutting stickers on a Silhouette Portrait 3.
Project 1 was vector layers, project 2 the Path tool. Project 4 is the machine.

## Intent

The artist draws a sticker and asks Umber for the line to cut it out by. Umber
answers with ordinary Cut paths on a vector layer: one closed, smooth outline
around each separate piece of artwork, standing off from the ink by a margin the
artist sets. They are then adjusted with the Path tool like any path drawn by
hand.

The artist approved this shape on 2026-11-26 ("Nice, continue") against this
proposal: source is everything visible or the selected layer or folder; one
closed outline per blob; a settable margin; holes ignored by default.

## Goals

- One command, "Trace outline…", that turns visible artwork into closed paths.
- A margin in document pixels, shown in millimetres as well.
- Curves, not staircases: a traced circle is a handful of nodes with handles.
- Paths land through the route every other path lands by, as one undo entry.
- The algorithm is pure CPU in `umber-core`, tested without a device.

## Non-goals (this project)

- A live preview. The result is one undo entry; retune by undoing and running
  again. The dialog remembers its settings for the session.
- Centre-line tracing for plot lines. Plot is offered as a role for the same
  outline, nothing more.
- Freehand drawing in the Path tool. The curve fitter built here is what it
  will use; it is the next piece of work, not this one.
- Smoothing or simplifying paths that already exist.
- Persisting the dialog's settings across runs.
- Clipping the result to the canvas. A margin may carry an outline past the
  edge; project 4 decides what a machine does with that.

## Anti-goals

- A cut line that crosses ink the artist can see.
- An outline of thousands of corner nodes nobody can edit.
- A white-backed document tracing as one rectangle the size of the page.
- A stray pixel becoming a sticker of its own.

## Approaches

1. **Distance transform and marching squares on the CPU, then curve fitting.**
   Read the stack's alpha once, threshold it, grow it by an exact Euclidean
   distance transform, contour at sub-pixel precision, fit cubics. Chosen:
   exact round offsets, testable with no device, and the fitter is reusable.
2. **Through the selection machinery.** Alpha to a `Selection`, then the
   existing "Cut path from selection". Rejected: selections have no grow, and
   `VectorObject::from_rings` deliberately does not smooth, so the result is
   the staircase.
3. **Read back the outline effect's GPU distance field.** Rejected: it ties a
   command to the effect cache's working set and budget, cannot be tested
   without a device, and still needs the contouring and fitting of approach 1.

## Design

### The algorithm: `umber-core::trace`

Input is a coverage grid (one byte per cell), the document position of its
first cell, and the size of a cell in document pixels (1 unless the caller
downsampled). Output is `Vec<Path>`, closed, in document pixels.

1. **Inside** is alpha at or above `THRESHOLD` (26 of 255, about 10%).
   Anything fainter is not printed strongly enough to matter and soft brush
   edges would otherwise push the line outwards by their whole falloff.
2. **Specks go.** A connected piece of inside (8-connected) smaller than
   `MIN_SPECK` cells (4) is removed before anything grows.
3. **Grow.** With a margin above zero, an exact Euclidean distance transform
   (Felzenszwalb and Huttenlocher, linear time) gives each cell its distance to
   the nearest inside cell, and the field contoured is `margin - distance`.
   With a margin of zero the field is `alpha - threshold`, so an antialiased
   edge places the line between pixels.
4. **Holes.** Unless holes are kept, every region of the grown shape's outside
   that cannot reach the grid's border is filled, so only outer outlines
   remain and nothing nested survives.
5. **Contour.** Marching squares at zero with linear interpolation along cell
   edges, on a grid padded so every contour closes. The ambiguous saddle is
   resolved by the cell centre's mean. Each ring is a closed polyline.
6. **Fit.** Corners are found where the outline turns by more than
   `CORNER_DEGREES` (60) across a short window; each run between corners is
   fitted with cubics by Schneider's algorithm to within the smoothing
   tolerance; a ring with no corner is split at one point and fitted with
   matching tangents at the seam, so the seam is a smooth node. A ring that
   fits to fewer than two nodes is dropped.

Rings are returned largest area first.

`trace` itself enforces no path or node ceiling; `VectorObject::appended`
already refuses a record past `MAX_PATHS` or `MAX_NODES` with its own
sentence, and the trace goes through it.

### Settings

`TraceSettings { margin: f32, smoothing: f32, keep_holes: bool, role: PathRole }`.

- Margin: 0 to `MAX_MARGIN` (512) document pixels.
- Smoothing: the fit tolerance, 0.25 to 8 pixels, default 1.
- Role: Cut or Plot. Ink is not offered.

### Reading the artwork

`CanvasRenderer::export_coverage(device, queue, layers) -> Vec<u8>`: one byte
of alpha per document pixel, from the same composite pass `export_rgba` runs,
with the document background left out for that pass. It is the screen's own
flattening, so masks, clipping, opacity and hidden layers are all honoured
with nothing restated. Blocking, as an export is; it runs on an explicit
command only.

The source is one of:

- **Everything**: `Editor::layer_draws`, as an export.
- **This layer**: the same draws with every draw outside the active entry's
  subtree made invisible. For a folder that is its contents. A layer hidden by
  its own eye or an ancestor's traces nothing.

A vector layer's Cut and Plot paths are an overlay and hold no pixels, so
paths already made are never traced.

The editor crops the coverage to the bounding box of what is inside, pads it
by the margin, and where that region is more than `MAX_CELLS` (8 million)
averages it down by a whole factor so the working grid fits. The factor goes
to `trace` as the cell size.

### The command

`Action::TraceOutline`, "Trace outline…", in the Edit menu directly under the
two "from selection" rows, unbound by default. It opens a modal drawn from
`ui::draw` like the canvas dialogs:

- Source: Everything / This layer.
- Role: Cut / Plot.
- Margin: a typed rail in pixels, with a dim line under it giving millimetres
  at the document's resolution. First opened in a session it starts at the
  pixel equivalent of 3 mm, clamped to the rail.
- Smoothing: a typed rail.
- Keep holes: a toggle, off.
- Trace and Cancel.

Trace puts down any float, settles a Path tool draft, finishes any stroke,
reads the coverage, runs `trace`, and lands the paths through
`Editor::plan_paths(paths, None)` and the same apply route
`App::path_from_selection` uses: appended to the active vector layer when it
can take them, otherwise a new layer named for the role. One undo entry.

Refusals are sentences in a notice and change nothing:

- Nothing found: "There is nothing to trace. The layers in view are empty."
  (For This layer: "There is nothing to trace on this layer.")
- Whatever `plan_paths` refuses, in its own words.

### Left to the builder

The internal shape of the fitter and the transform; the dialog's layout
within the house widgets; how the background is left out of one export pass;
where the session's settings live on `Editor::ui`.
