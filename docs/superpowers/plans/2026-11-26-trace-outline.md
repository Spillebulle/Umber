# Trace Outline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One command, "Trace outline…", that turns the visible artwork into smooth closed Cut or Plot paths standing off from the ink by a margin.

**Architecture:** A pure CPU module `umber-core::trace` turns a coverage grid into fitted paths (threshold, speck removal, exact distance transform, hole fill, marching squares, Schneider curve fit). `umber-render` gains one readback, the stack's alpha without the document background, through the existing export pass. `umber-app` adds the action, a modal, and lands the result through the existing `plan_paths` route.

**Tech Stack:** Rust, glam (`Vec2`, `UVec2`, `IVec2`), wgpu, egui. No new dependency.

**Spec:** `docs/superpowers/specs/2026-11-26-trace-outline-design.md` (on `main`; read it there or with `git show main:<path>`).

## Global Constraints

- Work in the worktree `C:/Users/spill/AppData/Local/Temp/umber-wt/vector`, branch `trace-outline`. Set `CARGO_TARGET_DIR` to `<worktree>/target` on every cargo invocation.
- Gates before every commit: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, and the tests of the crate touched.
- `umber-core` must not depend on wgpu, winit or egui. `umber-render` must not depend on winit or egui.
- Match enums exhaustively; never `matches!` where a `match` would fail the build on a new variant.
- Never hard-code a colour; never put a Unicode symbol in the interface; British spelling; no em-dashes in user-facing strings.
- Dialog widgets come from `widgets.rs` and `controls.rs`; a button strip is `controls::button_row` inside a `horizontal`.
- A refusal changes nothing and is a sentence.
- Do not edit `README.md`, `CHANGELOG.md` or `CLAUDE.md` except in Task 5.
- Do not drive a GUI. Do not run the application.
- Commit per coherent change, with a body saying why. End every commit message with:
  `Co-Authored-By: Claude Fable 5.2 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_019BvJmSpZKSQs3F4bMpF3XA`
- Constants, verbatim: `THRESHOLD = 26`, `MIN_SPECK = 4`, `MAX_MARGIN = 512.0`, `MAX_CELLS = 8_000_000`, `CORNER_DEGREES = 60.0`, smoothing range `0.25..=8.0` default `1.0`, default margin 3 mm at the document's dpi.

## Review Focus

- A white-backed document must trace the artwork, not the page. (Task 3 test.)
- Artwork touching the canvas edge must still give closed rings. (Task 1 test.)
- A region over `MAX_CELLS` is averaged down and the paths still land in document pixels at the right place. (Task 2 test.)
- Nothing in view, or an empty or hidden layer, is a sentence and no undo entry. (Task 4 test.)
- A margin wide enough to join two blobs gives one outline, not two crossing ones. (Task 1 test.)

---

### Task 1: Coverage to rings

**Files:**
- Create: `crates/umber-core/src/trace.rs` (or `trace/mod.rs` with `contour.rs` if it grows past about 600 lines)
- Modify: `crates/umber-core/src/lib.rs` (`pub mod trace;`)

**Interfaces:**
- Produces:
  ```rust
  pub const THRESHOLD: u8 = 26;
  pub const MIN_SPECK: usize = 4;
  pub const MAX_MARGIN: f32 = 512.0;
  /// One byte of alpha per cell, row-major, `size.x * size.y` long.
  pub struct Coverage<'a> { pub size: UVec2, pub alpha: &'a [u8] }
  /// Closed polylines in *cell* units, cell (0,0)'s centre at (0.5, 0.5).
  /// `margin` is in cells. Outer rings wind one way and holes the other;
  /// `ring_area` is positive for an outer ring.
  pub fn rings(cov: &Coverage, margin: f32, keep_holes: bool) -> Vec<Vec<Vec2>>;
  pub fn ring_area(ring: &[Vec2]) -> f32;
  ```
- A `Coverage` whose `alpha` length is not `size.x * size.y` returns no rings.

Steps (spec section "The algorithm", steps 1 to 5):

- [ ] **Step 1: Write failing tests** in `trace.rs`'s `tests` module, building grids by hand:
  - `an_empty_grid_has_no_rings`
  - `a_block_at_margin_zero_is_outlined_on_its_own_edge`: 20x20 grid, cells 5..15 at 255. One ring, bounds within 0.6 of (5,5)..(15,15), `ring_area` within 2% of 100.
  - `a_margin_grows_the_outline_by_a_round_offset`: same block, margin 6. Area within 3% of `100 + 40*6 + PI*36`; the ring's corner point is nearer `5 - 6/sqrt(2)` than `5 - 6` on each axis.
  - `a_faint_haze_is_outside`: cells at alpha 25 give no rings; at 26 they do.
  - `a_speck_is_dropped_and_a_small_mark_is_kept`: 3 connected cells give nothing even at margin 10; 4 give a ring.
  - `a_hole_is_filled_unless_asked_for`: a 30x30 block with a 10x10 hole: one ring with `keep_holes` false; two with true, one of negative area.
  - `a_piece_inside_a_hole_is_not_outlined_when_holes_are_filled`.
  - `artwork_touching_the_grid_edge_still_closes`: every cell at 255, margin 0 and margin 4: exactly one ring, first point connects to last, area at least the grid's.
  - `a_margin_that_joins_two_blobs_gives_one_ring`: two blocks 6 apart: two rings at margin 2, one at margin 4.
  - `an_antialiased_edge_places_the_line_between_pixels`: a column of alpha 128 beside a column of 255: the ring's x there is strictly between the two cell centres.
  - `a_grid_of_the_wrong_length_has_no_rings`.
- [ ] **Step 2: Run** `cargo test -p umber-core trace::` and see them fail to compile.
- [ ] **Step 3: Implement.** Threshold; 8-connected component sizes to drop specks; Felzenszwalb and Huttenlocher two-pass squared distance transform (exact), field `margin - sqrt(d2)` when margin > 0 and `(alpha - THRESHOLD + 0.5) / 255` when it is 0; pad the field by `ceil(margin) + 2` cells of "outside" on every side so contours close, and subtract the pad from the output; with `keep_holes` false, flood the outside from the padded border (4-connected over cells whose field is below zero) and raise every unreached negative cell to a small positive value; marching squares at zero with linear interpolation, the saddle settled by the mean of the four corners; stitch segments into closed rings.
- [ ] **Step 4: Run** the tests to green, then the gates.
- [ ] **Step 5: Commit.**

### Task 2: Rings to paths

**Files:**
- Modify: `crates/umber-core/src/trace.rs` (or add `trace/fit.rs`)

**Interfaces:**
- Consumes: `rings`, `ring_area`, `Coverage`; `umber_core::vector::{Path, Node, PathRole}`; `umber_core::pathedit::is_smooth`.
- Produces:
  ```rust
  pub const MAX_CELLS: usize = 8_000_000;
  pub const CORNER_DEGREES: f32 = 60.0;
  pub const SMOOTHING: std::ops::RangeInclusive<f32> = 0.25..=8.0;
  #[derive(Clone, Copy, Debug, PartialEq)]
  pub struct TraceSettings { pub margin: f32, pub smoothing: f32, pub keep_holes: bool, pub role: PathRole }
  impl TraceSettings { pub fn new(role: PathRole) -> Self } // margin 0, smoothing 1.0, holes off
  /// Cubic nodes through an open run of points, within `tolerance`.
  pub fn fit_open(points: &[Vec2], tolerance: f32) -> Vec<Node>;
  /// Cubic nodes round a closed ring, within `tolerance`.
  pub fn fit_closed(ring: &[Vec2], tolerance: f32) -> Vec<Node>;
  /// The part of a document's alpha worth tracing, ready for `trace`.
  pub struct Region { pub origin: IVec2, pub cell: u32, pub size: UVec2, pub alpha: Vec<u8> }
  /// Crop `alpha` (document-sized) to what is inside, pad by `margin`, and
  /// average down by a whole factor until it is at most `max_cells`.
  /// `None` when nothing reaches `THRESHOLD`.
  pub fn region(doc: UVec2, alpha: &[u8], margin: f32, max_cells: usize) -> Option<Region>;
  /// Closed paths in document pixels, largest first.
  pub fn trace(region: &Region, settings: &TraceSettings) -> Vec<Path>;
  ```
- `trace` clamps `margin` to `0.0..=MAX_MARGIN` and `smoothing` to `SMOOTHING`, converts margin to cells by dividing by `region.cell`, and maps ring points back with `origin + point * cell`.

- [ ] **Step 1: Write failing tests:**
  - `a_circle_fits_in_a_handful_of_smooth_nodes`: sample a radius-100 circle at 1 px spacing; `fit_closed(.., 1.0)` gives at most 12 nodes, every node `is_smooth`, and every point of the flattened path (`Path::flatten` at 0.1) is within 1.5 of the circle.
  - `a_square_keeps_its_four_corners`: a 100-square ring sampled every pixel gives exactly 4 nodes, each within 0.5 of a corner, none `is_smooth`.
  - `a_fit_stays_within_its_tolerance`: a wobbly ring (`r = 80 + 10 sin 5θ`) at tolerances 0.5, 2 and 8: every input point is within tolerance + 0.5 of the flattened result, and a looser tolerance never gives more nodes.
  - `an_open_run_keeps_its_ends`: `fit_open` first and last node sit exactly on the first and last point.
  - `a_ring_too_small_to_fit_is_dropped` (fewer than 3 points gives no nodes; `trace` drops it).
  - `a_traced_disc_lands_where_it_was_drawn`: 400x300 document, a filled disc radius 40 at (250,120), `region` then `trace` at margin 10: one closed path of role Cut, bounds within 1.5 of centre ± 50, at most 16 nodes.
  - `a_region_over_the_ceiling_is_averaged_down_and_still_lands_in_place`: same picture with `max_cells = 5_000`: `cell > 1`, and the path's bounds within `cell + 1.5` of the same answer.
  - `nothing_inside_is_no_region`.
  - `paths_come_largest_first`.
  - `a_margin_past_the_ceiling_is_clamped`.
- [ ] **Step 2: Run** and see them fail.
- [ ] **Step 3: Implement.** Corner detection: at each ring vertex compare the direction over a window of about 4 pixels of arc either side; a turn above `CORNER_DEGREES` is a corner, non-maximum suppressed within the window. Runs between corners go to `fit_open` (Schneider: chord-length parameterisation, least-squares handles from end tangents, one Newton reparameterisation pass, split at the worst point when over tolerance). Corner nodes get the handles their two runs give them; interior split nodes are smooth (collinear handles). A ring with no corner is opened at vertex 0, fitted with both end tangents set to the centred tangent there, and closed. `region` averages with whole factors 2, 3, 4… and keeps `origin` in document pixels.
- [ ] **Step 4: Run** to green, then the gates.
- [ ] **Step 5: Commit.**

### Task 3: The stack's alpha, without the page

**Files:**
- Modify: `crates/umber-render/src/canvas.rs`
- Test: `crates/umber-render/tests/gpu_pipeline.rs`

**Interfaces:**
- Produces: `pub fn export_coverage(&self, device: &wgpu::Device, queue: &wgpu::Queue, layers: &[LayerDraw]) -> Vec<u8>`: `doc.x * doc.y` bytes, the alpha `export_rgba` would give for a transparent-backed document.

- [ ] **Step 1: Write failing GPU tests** (skip without an adapter, as the others do):
  - `coverage_of_a_white_backed_document_is_the_artwork_alone`: `set_background` opaque white, one dab committed; `export_coverage` is 0 far from the dab and 255 at its centre, while `export_rgba`'s alpha is 255 everywhere.
  - `coverage_is_the_alpha_an_export_would_have_without_a_background`: transparent background: every byte equals the alpha channel of `export_rgba`.
  - `a_hidden_layer_has_no_coverage`.
- [ ] **Step 2: Run** `cargo test -p umber-render --test gpu_pipeline coverage` and see them fail.
- [ ] **Step 3: Implement** through `render_export` and `read_texture_rows`, with the background left out for that one pass (a parameter on the shared internal function, not a second pass and not a mutation of `self.background`). `export_rgba` must be byte for byte what it was.
- [ ] **Step 4: Run** the whole `gpu_pipeline` suite, then the gates.
- [ ] **Step 5: Commit.**

### Task 4: The command

**Files:**
- Modify: `crates/umber-app/src/editor.rs`, `app.rs`, `shortcuts.rs`, `ui.rs` (menu row only)

**Interfaces:**
- Consumes: `umber_core::trace::{region, trace, TraceSettings, MAX_CELLS}`; `CanvasRenderer::export_coverage`; `Editor::plan_paths`, `Editor::layer_draws`, `App::carry_out_path_plan`, `App::finish_stroke`, `App::finish_transform`, and whatever `App::path_from_selection`'s siblings call to settle a Path tool draft before a path lands (see `float_a_clip`).
- Produces:
  ```rust
  // editor.rs
  #[derive(Clone, Copy, Debug, PartialEq, Eq)]
  pub enum TraceSource { Everything, ThisLayer }
  impl TraceSource { pub const ALL: [TraceSource; 2]; pub fn label(self) -> &'static str } // "Everything", "This layer"
  #[derive(Clone, Debug)]
  pub struct TraceForm { pub open: bool, pub source: TraceSource, pub settings: TraceSettings, pub seeded: bool }
  // on Editor::ui (above the documents line): pub trace: TraceForm
  // on Editor: pub trace_requested: bool  (the dialog sets it; App::render spends it)
  impl Editor {
      /// The draws a trace reads: all of them, or only the active entry's subtree visible.
      pub fn trace_draws(&self, source: TraceSource) -> Vec<LayerDraw>;
      /// Plan the paths a trace of `alpha` (document-sized coverage) makes. Changes nothing.
      pub fn plan_trace(&self, alpha: &[u8], source: TraceSource, settings: &TraceSettings) -> Result<PathPlan, Notice>;
      /// Open the dialog, seeding the margin to 3 mm at the document's dpi the first time.
      pub fn open_trace(&mut self);
  }
  // app.rs
  fn trace_outline(&mut self); // settle, read coverage, plan, carry out
  // shortcuts.rs
  Action::TraceOutline // label "Trace outline…" spelt with three full stops if the house style has no ellipsis glyph: check how other dialog-opening actions are labelled and match them; group "Edit"; unbound
  ```
- Notices, verbatim: title "Nothing to trace"; line for Everything: "The layers in view are empty."; for This layer: "There is nothing to trace on this layer."

- [ ] **Step 1: Write failing tests** in `editor.rs` (no device; build `alpha` by hand):
  - `a_trace_of_a_disc_plans_one_cut_path` (new layer when the active layer is raster; append when it is an editable vector layer).
  - `a_trace_of_nothing_is_a_sentence_and_changes_nothing`, for both sources, asserting the exact lines and that history length and `modified` are unchanged.
  - `trace_draws_for_this_layer_hide_everything_outside_it`: three layers, the middle active: only its draw is visible; with a folder active, its contents are; a layer whose own eye is off stays invisible.
  - `trace_draws_for_everything_are_the_layer_draws`.
  - `opening_the_dialog_seeds_three_millimetres_once`: dpi 300 gives a margin of 35.0 (`round(300 * 3 / 25.4)`); changing it and reopening keeps the change.
  - `the_trace_action_is_listed_unbound_under_edit` in `shortcuts.rs`, and extend the exhaustive `ALL` guard.
  - A source scan, in the style of `no_float_is_begun_with_the_path_tool_in_hand`, that `fn trace_outline` calls the stroke, float and draft settlers before `export_coverage`.
- [ ] **Step 2: Run** and see them fail.
- [ ] **Step 3: Implement.** `Action::TraceOutline` calls `Editor::open_trace`. `App::render` (or wherever `quit_requested`-style flags are spent) takes `trace_requested` and calls `trace_outline`, which settles, calls `export_coverage` with `trace_draws`, then `plan_trace`, then `carry_out_path_plan(plan, plan.adds_a_layer())`, and closes the dialog on success. Add the menu row with `menu_item` under the two from-selection rows.
- [ ] **Step 4: Run** `cargo test -p umber-app --lib`, then the gates.
- [ ] **Step 5: Commit.**

### Task 5: The dialog and the documents

**Files:**
- Create: `crates/umber-app/src/tracedlg.rs`
- Modify: `crates/umber-app/src/lib.rs` (module), `ui.rs` (draw it from `ui::draw` beside the canvas dialogs), `CLAUDE.md`, `README.md`, `CHANGELOG.md`

**Interfaces:**
- Consumes: `Editor::ui.trace: TraceForm`, `Editor::trace_requested`, `TraceSource`, `umber_core::trace::{MAX_MARGIN, SMOOTHING}`, `widgets::{segmented, typed_row or number_row, toggle}`, `controls::button_row`, `tabs::dialog_frame`.
- Produces: `pub fn show(ctx: &egui::Context, ed: &mut Editor)`.

Contents, top to bottom: heading "Trace outline"; Source segmented (Everything / This layer); Role segmented (Cut / Plot); Margin typed rail, 0 to `MAX_MARGIN`, suffix " px", and under it a dim line "`{mm:.1} mm at {dpi:.0} dpi`"; Smoothing typed rail over `SMOOTHING`; "Keep holes" toggle; Cancel and Trace buttons. Escape and Cancel close it and change nothing. Trace sets `trace_requested`.

- [ ] **Step 1: Write failing headless tests** (`egui::Context::run_ui`, fonts and theme installed as the other dialog tests do), reading the shapes drawn:
  - `the_dialog_draws_every_control_it_names` (each label above is present).
  - `the_millimetre_line_follows_the_margin_and_the_resolution` (35 px at 300 dpi reads "3.0 mm at 300 dpi").
  - `cancel_changes_nothing_and_trace_asks_once`.
  - `no_two_buttons_in_the_trace_dialog_touch` (measure the rectangles).
  - `the_dialog_offers_no_ink_role`.
- [ ] **Step 2: Run** and see them fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Documents.** `CLAUDE.md`: a "#### Trace outline" section after "#### The Path tool", in the file's own voice, covering: why CPU and not the effect's distance field; why the background is left out of the pass rather than subtracted; the threshold and speck rules and what each prevents; that holes are filled on the grown shape; that the fitter is the one freehand drawing will use; what is not built (preview, centre line, clipping to the canvas). `README.md`: one or two sentences in the paths part of the feature list. `CHANGELOG.md`: a line under the current unreleased version's section.
- [ ] **Step 5: Run** the whole workspace's tests and the gates.
- [ ] **Step 6: Commit** (code and documents as separate commits).
