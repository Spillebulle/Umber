# The Path tool: drawing and editing cut and plot lines by hand

Date: 2026-11-25. Project 2 of four on the way to cutting stickers on a
Silhouette Portrait 3. Project 1 (vector layers, branch `vector-layers`) is
built and is what this stands on. Project 3 generates paths from artwork;
project 4 is printing, registration marks and the machine.

## Intent

The artist wants to "draw cutting lines, or automatically generate them, and
easily edit them, same with plotting lines". Today a path can only come from a
selection's outline, as straight segments, and once made it cannot be changed
at all. This project adds the hand: draw a path point by point with curves,
and pick any path up afterwards and reshape it.

It matters twice. A cut line drawn by hand is the direct route for a simple
sticker. And every generated line (project 3) will be nearly right rather than
right, so editing is what makes generation usable.

## Goals

- Draw a new cut or plot path on the canvas, with straight and curved
  segments, open or closed.
- Select an existing path and change it: move nodes, bend segments with
  handles, add a node, remove a node, move the whole path, delete it, open or
  close it, switch it between cut and plot.
- Paths made from a selection (project 1) are editable by exactly the same
  means. There is one kind of path.
- Mouse and pen both work, through `gesture.rs`.
- Every finished gesture is one undo step.

## Non-goals (this project)

- Freehand drawing fitted to a curve. Deferred; it belongs with project 3's
  curve fitting, which it would share.
- Rectangle and ellipse path shapes. The Select tool's shapes plus "Cut path
  from selection" already give them, as straight segments; turning those into
  true curves is project 3's fitting again.
- Selecting several nodes or several paths at once, a marquee over nodes,
  arrow-key nudging, snapping, alignment, numeric node positions.
- Editing paths on any layer but the active one.
- Ink paths. Still reserved and still not drawn.
- Any change to the file format. A node already stores both handles.

## Anti-goals

- A press that silently does something destructive. No gesture here may
  remove or reshape a path without an undo entry.
- A dialog on a canvas press. Refusals are a sentence on the tool options
  strip.
- A mark drawn where the hit test disagrees with it.
- Any work per frame that grows with the document rather than with what is
  on screen and selected.

## Approaches considered

1. **One tool with two modes, Draw and Edit (chosen).** A segmented switch on
   the options strip. In Draw a press on empty canvas places a node; in Edit
   it lets go of the selection. Holding Ctrl in Draw borrows Edit for one
   gesture. It keeps "what does a press on empty canvas mean" unambiguous,
   which is the question the other two approaches trip on, and it is one rail
   button and one shortcut.
2. **Two tools, Pen and Node.** What Illustrator and Inkscape do. Same model
   underneath, but two rail buttons, two shortcuts and a tool switch for the
   most common loop (place, adjust, place). Rejected as more interface for the
   same behaviour.
3. **One modeless tool.** Everything decided by what is under the pointer.
   Fewest controls, but an empty press must either start a path (so there is
   no way to click away from a selection, and stray paths get made) or
   deselect (so drawing needs a modifier). Rejected.

Freehand-first was the fourth candidate and is deferred as above: the artist
was asked and passed the choice back, so point-by-point is my call. It is the
precise one, and the one that needs no fitting code to exist first.

## The design

### Where things live

- `umber-core::pathedit` (new): pure geometry on `Path` and `VectorObject`.
  Hit testing, splitting a segment, removing a node, the handle rules. No
  camera, no egui.
- `umber-app::pathtool` (new): the tool's model. A state machine fed presses,
  moves, releases and keys in document space, answering what to draw and what
  to commit. No winit, no drawing. Same division as `layerdrag.rs`.
- `gesture.rs`: `Tool::Path` resolves to a new `Press::Path`. No `match` on
  `Tool` in either event arm.
- `app.rs`: routes `Press::Path`, a new `Interaction::Pathing` while the
  pointer is held, and the keys.
- `ui.rs`: paints the draft, the drag preview, the node and handle marks, and
  the options strip.
- `editor.rs`: owns the `PathTool` state and turns its commits into record
  swaps and undo entries.

### State

`PathMode { Draw, Edit }` and the role for new paths (`Cut` by default) are
tool settings, above the documents line, not saved to preferences in this
project.

A **draft** is a path being drawn that is on no layer yet. A **pick** is the
selected path on the active layer and, optionally, one node of it. A pick is
made only by an Edit gesture (Edit mode, or Ctrl in Draw). Both belong to the
document in front.

- **The draft is settled** by a change of tool, a tab switch, closing the
  document, and a canvas resize or flip. Settling **keeps it** when it has
  two or more nodes (committed as an open path) and drops it otherwise. Where
  it cannot be kept (see "Where a path lands") the settle raises a notice
  titled "The path being drawn was not kept" with the reason, and drops it.
  That is the one dialog in this design and it is never on a canvas press. A
  save, an export and a change of active layer leave the draft alone: it is
  on no layer and in no file until it is finished.
- **The pick is let go** by all of those and also by a change of active
  layer, and by any change to the stack's `vector_revision` the tool did not
  make itself (an undo, a redo, a delete).
- **Undo and redo while a draft is up belong to the draft.** Undo takes its
  last node off, exactly as Backspace does. Redo does nothing. Neither
  touches the document's history, so nothing is committed and no redo stack
  is lost. With no draft they are the ordinary commands.

A pick names a path by index.

### Draw mode

- Press on the canvas places a node. Dragging before release pulls its
  handles out: `handle_out` is the drag, `handle_in` its negative. A release
  within `gesture::TAP_SLOP` leaves a corner.
- Between presses a rubber-band segment follows the pointer, pen hover
  included, from the last node, curved by that node's `handle_out`.
- Pressing on the first node (within the hit radius) closes the path and
  finishes it. A closed path needs three nodes; with fewer the press is an
  ordinary node.
- Enter finishes the draft as an open path. It needs two nodes.
- Backspace takes the last node off the draft. Taking the only node off ends
  the draft.
- Escape abandons the draft. That is what Escape does to every other outline
  being drawn in Umber, and the hint says so.
- **Continuing a path.** With no draft up, a press on an end node of an open
  path on the active layer (where that layer may be edited, see Edit mode)
  takes that path up as the draft, reversed if it was the first node that
  was pressed, so new nodes go on at that end. Finishing then **replaces**
  that path in the record rather than appending a second one, and abandoning
  leaves it as it was.
- Any other plain press with no draft up lets go of the pick and starts a
  new draft.
- Finishing a path does not pick it and leaves the tool in Draw.
- Ctrl held at a press makes that one gesture an Edit gesture on committed
  paths. The draft's own nodes are not editable until it is finished.
- Delete and Backspace: with a draft up they take its last node off; with no
  draft and a pick, they act on the pick as in Edit mode.

### Where a path lands

Project 1's rule, shared rather than restated: appended to the active layer
if that is a visible, unlocked vector layer whose record this build can
change, otherwise on a new vector layer above the active entry. A hidden
layer counts as one that cannot take it, so nothing is ever added where it
would not be drawn. `Editor::plan_path_from_rings` is generalised to take
finished paths, and both callers use it. One undo entry: `Paths` for an
append or a replacement, `AddLayer` for a new layer.

A path that cannot land (a full stack, no layer slice, too many paths or
nodes) is refused. On the closing press or Enter the draft **stays up** and
the strip carries the reason, so nothing drawn is lost and the artist can
make room. The planner returns the reason as a sentence; the selection
commands go on wrapping it in their notice.

### Edit mode

Only the active layer's paths can be hit, and only when it is a vector layer
that is visible, unlocked and not `Opaque`. Otherwise every press does nothing and the
strip says why.

A press is resolved in this order, first match wins. Radii are screen
distances (8 points) divided by the zoom.

1. A handle of the picked node: drag it.
2. A node of the picked path: pick that node and drag it.
3. A segment of the picked path: a release within the tap slop **inserts a
   node** at that point and picks it; a drag **moves the whole path**.
4. A segment of any other path on the layer: pick that path. (No node
   picked.) Where several are within reach, the nearest.
5. Nothing: let go of the pick.

The rules:

- **Moving a node** carries its handles, which are offsets.
- **Moving a handle** on a smooth node turns the opposite handle to stay in
  line with it, keeping the opposite handle's own length. A node is smooth
  when both handles are non-zero and within 1 degree of opposite at the
  press. Holding Shift moves the one handle alone, which is how a smooth node
  becomes a cusp. (Alt is the eyedropper on every tool and is not used.)
- **Inserting a node** splits the cubic by de Casteljau at the nearest
  parameter, so the curve does not move. Splitting a straight segment gives a
  corner.
- **Delete or Backspace** with a node picked removes that node; the
  neighbours keep their own handles. A path left with fewer than two nodes,
  or a closed path left with fewer than three, is removed whole. With a path
  picked and no node, it removes the path.
- **Escape** lets go of the node, then of the path.
- The **Cut / Plot** switch shows the picked path's role while there is a
  pick and the role for new paths otherwise. Pressed with a pick it changes
  that path alone; pressed without one it changes the role for new paths.
  Picking a path never changes the role for new paths. With a pick the strip
  also carries **Close path** or **Open path**, in either mode. For a picked node: **Corner** (both handles to zero) and
  **Smooth** (handles set along the line between its two neighbours, each a
  third of the distance to its own neighbour; an end node of an open path is
  left alone and the button is disabled there).
- Every one of these is one `Edit::vector` entry, "Edit paths". A drag that
  ends where it began, and a strip press that changes nothing, record
  nothing.
- A result the record would refuse (a coordinate past `MAX_COORD`, a node
  count past `MAX_NODES`) is not applied; the path stays as it was and the
  strip says so until the next press.

### What is drawn

- **The layer's record is not written during a drag.** The original stays
  drawn by the existing overlay, and the edited path is drawn over it in the
  accent pair, from the tool's own copy. The record is swapped once, at the
  release. This keeps the overlay cache and `vector_revision` out of the
  pointer path, makes "one gesture, one undo entry" structural, and means an
  autosave can never see a half-dragged path. `Interaction::Pathing` is still
  not idle, so no autosave starts mid-drag.
- The draft is drawn the same way, with its rubber band.
- The picked path is redrawn in the accent pair so it reads apart from its
  neighbours. Its nodes are small squares, the picked node filled; the picked
  node's handles are a thin line to a small circle each. Dark-then-light pair
  from `Palette::accent` and `accent_underlay`, widths rounded to whole
  device pixels as `ui::ant_width` does.
- **Marks and hit test share one predicate.** Node marks are culled to the
  view. If more than 2048 of the picked path's nodes fall in view, no node
  marks are drawn, node and handle hits are off (segment hits still work),
  and the strip says to zoom in. A path traced from a lasso can hold tens of
  thousands of nodes, and a mark per node per frame is the cost the standing
  rule forbids.
- The preview is flattened with the overlay's own tolerance and clip, into a
  buffer the tool keeps, so nothing is allocated per frame once it has grown.

### Controls

- `Tool::Path`, label "Path", on the tool rail after Transform. Lucide's
  `pen-tool` mark. `Action::PathTool`, bound to `P` if nothing holds it,
  unbound otherwise.
- Options strip: the Draw / Edit switch, the Cut / Plot switch, the buttons
  above when they apply, then one hint sentence that follows the state. The
  strip is one unwrapped row, so each group gets a measured budget and the
  hint is what gives way.
- The cursor stays the ordinary arrow.

### Refusal sentences (strip, not dialogs)

- Edit, active layer not a vector layer: "Select a vector layer to edit its
  paths."
- Edit, locked: "This layer is locked."
- Edit, hidden: "This layer is hidden. Show it to edit its paths."
- Edit, `Opaque`: "These paths are from a newer Umber and cannot be changed
  here."
- Too many nodes in view: "Zoom in to edit this path's nodes."
- A result the record refuses: the record's own `VectorRefusal` sentence.

## Constraints

Everything in `CLAUDE.md` binds. The ones this design leans on: gestures are
decided in `gesture.rs`; exhaustive matches, no `matches!` where a `match`
would fail the build; no hard-coded colours; Lucide icons, pasted; British
spelling and no em-dashes in interface text; `umber-core` free of egui and
wgpu; nothing allocates per frame on the drawing path; a model is tested as a
model and a panel by what egui drew; no test asserts wall-clock time.

`vector::VERSION` stays 1 and `umber-version` stays 5.

## Left to the builder

- Exact names and signatures inside `pathedit` and `pathtool`.
- Whether the preview reuses `vectorview`'s clip through a `pub(crate)` door
  or a small shared helper, provided there is still one flattener.
- Mark sizes in points, within what reads at 100% and 200%.
- The strip's budgets, measured.
- Whether `P` is free.

## Not verifiable here

Nobody building this can look at a window or hold a pen. Colours and weights
of the marks over real artwork, the feel of the hit radius, and the pen path
are reasoned and tested as models, and need the artist's eye afterwards.

## As built: where the tool differs from the text above

Recorded after the build on branch `path-tool`. Each of these was decided
during the build without the artist; any of them can be reversed.

- **A canvas flip mirrors a draft in hand.** The text above says anything that
  moves the canvas settles the draft first. A flip reached through the History
  panel cannot settle first, so the draft is mirrored with the picture.
- **Closing a tab or the window lands the draft before the prompt appears.**
  Pressing Cancel on that prompt leaves the path on the layer rather than back
  in hand.
- **Taking a path up and adding nothing changes nothing.** No undo entry, no
  unsaved mark, and the path is not reversed.
- **Undo into the part of a continued path that was already there abandons the
  whole draft**, leaving the path as it was.
- **A continued draft stops being a continuation** when the layer in hand
  changes, when the path it was taken from is no longer the same path, or when
  that path is edited with Ctrl held. It then lands as a new path.
- **A continued draft keeps the role of the path it came from**, whatever the
  Cut and Plot switch shows.
- **A drag that would put a path too far from the canvas** is refused with the
  tool's own sentence: "That would put the path too far from the canvas."
- **Ctrl with a press on a layer that cannot be edited says why** on the strip.
- **Escape during a drag cancels the drag only.** A second Escape abandons the
  draft.
- **Enter with a single point down does nothing.**
- **"Close path" is drawn disabled, with a tooltip, under three points**,
  rather than not drawn.
- **The selected path stays selected when an edit is refused**, and moves with
  the path when an edit reorders the record.
- **A frame in which nothing changed does no work over the selected path.**
  The preview is rebuilt when the path, the selection, the camera or the
  pointer's aim changes, and that rebuild allocates.
- **A new layer that finds no room** shows the existing remedy's wording
  rather than a sentence of this tool's own.
