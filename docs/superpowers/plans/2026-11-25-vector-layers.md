# Vector Layers (stage 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A layer that holds cut and plot paths: it saves, reopens, undoes, draws its lines over the canvas, and can be filled from the selection's outline.

**Architecture:** A `VectorObject` record on an ordinary slot-holding `Layer`, threaded exactly where `TextObject` is threaded (stack, undo, `.ora` writer and reader, both savers). Cut and Plot paths never rasterise; `umber-app` draws them as an egui overlay from one flattener in `umber-core`. Ink paths are reserved in the model and the format and have no control anywhere.

**Tech Stack:** Rust, `glam`, `serde`/`serde_json`, egui. No new dependency. No shader changes.

**Spec:** `docs/superpowers/specs/2026-11-25-vector-layers-design.md`. Read it and the repository's `CLAUDE.md` before any task. Where this plan and the spec disagree, the spec wins; say so in your report.

## Global Constraints

- `umber-core` gains no GPU, winit or egui type. `composite.wgsl`, `commit.wgsl`, `blend.wgsl` are untouched.
- Every `match` on `PathRole`, `VectorObject`, `EditKind`, `EditBody`, `EditRefusal` is exhaustive. No `matches!`, no `_ =>`.
- No hard-coded colour; no Unicode symbol in the interface; a new `Icon` is a Lucide paste.
- User-facing strings: short sentences, British spelling, no em-dashes.
- Nothing on the drawing path allocates per frame.
- Do not edit `CLAUDE.md`, `README.md` or `CHANGELOG.md` until Task 8. Report prose instead.
- Never edit a file with PowerShell `Get-Content | Set-Content`.
- Commit before mutating code to test a guard. One commit per coherent change, message says what and why, ending with the two attribution lines the session supplies.
- Do not drive a GUI. Use headless `egui::Context::run_ui` tests.
- Gates before each task is reported done: `cargo fmt --all`, `cargo clippy --workspace --all-targets` (zero warnings), `cargo test` for the crates touched. Run `rustup update` once before the first clippy.
- A test that writes a process-global takes that global's lock (`prefs_lock`, `gputest::lock`).
- Mutate every new guard once and confirm it fails; say in the report which mutation.

## Review Focus

1. **A record with a non-finite or absurd figure in a stranger's file** (NaN position, 10^30 handle): must be refused on read, never reach `flatten` or a vertex. Test in Task 1.
2. **A path far outside the canvas or the view at high zoom**: the overlay must cull or clamp rather than emit millions of segments or overflowing coordinates. Test in Task 7.
3. **Undoing a path edit after the layer was reordered, or into a folder**: the entry names a slot, and must land on the same layer wherever it now sits. Test in Task 3.
4. **Autosave of a document with a vector layer**: must write the same record Save does; a missing field writes `None` silently. Test in Task 5.
5. **The command with the active layer locked, a folder selected, or the stack full**: a sentence, and nothing changed. Test in Task 6.

---

### Task 1: The model and its record, `umber-core::vector`

**Files:**
- Create: `crates/umber-core/src/vector.rs`
- Modify: `crates/umber-core/src/lib.rs` (`pub mod vector;`, re-export `VectorObject, Path, Node, PathRole`)

**Interfaces — Produces:**

```rust
pub const VERSION: u32 = 1;
pub const MAX_NODES: usize = 65_536;   // per layer
pub const MAX_PATHS: usize = 4_096;    // per layer
pub const MAX_COORD: f32 = 1.0e6;      // |any figure| bound, document px
pub const MAX_RECORD_BYTES: usize;     // derived: see step 5

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathRole { Cut, Plot, Ink }
impl PathRole { pub const ALL: [PathRole; 3]; pub fn label(self) -> &'static str; } // "Cut", "Plot", "Ink"

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Node { pub at: Vec2, pub handle_in: Vec2, pub handle_out: Vec2, pub pressure: f32 }
impl Node { pub fn corner(at: Vec2) -> Self; } // zero handles, pressure 1.0

#[derive(Clone, Debug, PartialEq)]
pub struct Ink { pub brush: String, pub size: f32, pub colour: [f32; 4] }

#[derive(Clone, Debug, PartialEq)]
pub struct Path { pub role: PathRole, pub closed: bool, pub nodes: Vec<Node>, pub ink: Option<Ink> }
impl Path {
    pub fn flatten(&self, tolerance: f32, out: &mut Vec<Vec2>); // appends; clears nothing
    pub fn bounds(&self) -> Option<(Vec2, Vec2)>;               // of nodes and handles
}

#[derive(Clone, Debug, PartialEq)]
pub enum VectorObject { Paths(Vec<Path>), Opaque(Vec<u8>) }
impl VectorObject {
    pub fn empty() -> Self;                              // Paths(vec![])
    pub fn paths(&self) -> &[Path];                      // &[] for Opaque
    pub fn is_opaque(&self) -> bool;
    pub fn has_ink(&self) -> bool;
    pub fn node_count(&self) -> usize;
    pub fn byte_len(&self) -> usize;
    pub fn check(&self) -> Result<(), VectorRefusal>;    // bounds + finiteness
    pub fn appended(&self, more: Vec<Path>) -> Result<Self, VectorRefusal>;
    pub fn flipped(&self, axis: FlipAxis, canvas: UVec2) -> Self;   // Opaque: unchanged clone
    pub fn translated(&self, by: Vec2) -> Self;                     // Opaque: unchanged clone
    pub fn without_ink(&self) -> Self;
    pub fn from_rings(rings: &[Vec<Vec2>], role: PathRole) -> Vec<Path>;
    pub fn to_json(&self, print: Option<&Fingerprint>) -> Result<Vec<u8>, VectorRefusal>;
    pub fn from_json(bytes: &[u8]) -> Result<(Self, Option<Fingerprint>), RecordError>;
}
pub enum VectorRefusal { TooManyNodes, TooManyPaths, Impossible, Opaque }
impl VectorRefusal { pub const ALL: [..; 4]; pub fn sentence(self) -> &'static str; }
pub enum RecordError { TooLarge, Unreadable(String) }
```

`Fingerprint` is `crate::textobj::Fingerprint`, reused. `FlipAxis` is `crate::geom::FlipAxis`.

Decisions the signatures leave open:

- **Flatten:** per cubic, segment count `n = ceil(sqrt(d / (8/6 * tolerance)))` clamped to `1..=256`, where `d` is the larger of `|p0 - 2p1 + p2|` and `|p1 - 2p2 + p3|` (the standard second-difference bound). Zero handles on both ends emit only the end point. A closed path emits the closing segment and repeats the first point last. `tolerance` is clamped to at least `1e-3`.
- **`from_rings`:** one closed path of `Node::corner`s per ring; drop a node exactly collinear with its neighbours (cross product `== 0.0`) and exact duplicates; drop a ring left with fewer than three nodes.
- **`flipped`:** horizontal maps `x -> canvas.x - x` and negates handle x; vertical the same on y. In and out handles are not swapped.
- **`check`:** every `at`, handle, `pressure`, `Ink::size`, `Ink::colour` finite and `|v| <= MAX_COORD`; every path has at least 2 nodes; counts within `MAX_NODES`/`MAX_PATHS`. `Opaque` passes.
- **Record JSON**, compact, field order fixed:
  `{"version":1,"paths":[{"role":"Cut","closed":true,"nodes":[{"at":[x,y],"in":[x,y],"out":[x,y],"pressure":1.0}],"ink":null}],"print":…}`.
  `in`, `out` default to `[0,0]`, `pressure` to `1.0`, `closed` to `false`, `ink` and `print` to absent, each by its **own** `#[serde(default)]`; `role`, `at`, `version`, `paths` have none. `ink` and `print` are omitted when `None` (`skip_serializing_if`). `to_json` writes `print` only when `has_ink()` and one was given.
- **`from_json`:** over `MAX_RECORD_BYTES` is `TooLarge`. `version > VERSION` returns `Ok((Opaque(bytes.to_vec()), None))`. Parse failure or a failed `check` is `Unreadable`. Paths with fewer than 2 nodes are dropped rather than refusing the record.
- **`to_json` of `Opaque`** returns the bytes verbatim.
- **`VectorRefusal::sentence`:** `TooManyNodes` "This layer cannot hold that many points. Simplify the shape or use another vector layer."; `TooManyPaths` "This layer cannot hold more paths. Use another vector layer."; `Impossible` "That shape could not be turned into a path."; `Opaque` "This layer's paths were made by a newer Umber and cannot be changed here."

- [ ] **Step 1: Write the failing tests** in `vector.rs`'s `tests` module:

```rust
a_straight_path_flattens_to_its_nodes            // 3 corners, open -> exactly 3 points
a_closed_path_returns_to_its_start               // last == first
a_quarter_circle_flattens_within_tolerance       // kappa 0.5522848 handles, r=100; every point within r±(0.02+tol) for tol in [1.0, 0.25, 0.05]; finer tol gives >= as many points
flattening_is_bounded                            // handles of 1e6 -> at most 256 points per segment
flipping_twice_is_the_identity                   // both axes, exact ==, canvas 700x500
a_flip_mirrors_about_the_canvas                  // at (10,20) -> (690,20); handle (3,4) -> (-3,4)
translating_moves_nodes_and_not_handles
a_rectangles_rings_become_four_nodes             // ring with collinear midpoints on each edge
a_ring_with_two_distinct_points_is_dropped
no_role_is_missing_from_all                      // exhaustive match indexing ALL
the_record_is_exactly_this_text                  // pinned literal for one Cut path, one Plot path with a curved node; no "print", no "ink"
a_record_round_trips
a_path_with_no_role_is_refused                   // Unreadable
a_nan_or_huge_figure_is_refused                  // "at":[1e30,0] and a NaN via 1e999 -> Unreadable
a_newer_record_is_kept_whole                     // {"version":2,"shape":"x"} -> Opaque, to_json returns identical bytes
a_record_over_the_bound_is_refused_unparsed
a_print_is_written_only_beside_ink
too_many_nodes_is_refused_and_changes_nothing    // appended() at MAX_NODES+1
an_opaque_record_cannot_be_appended_to
the_byte_bound_admits_the_largest_legal_record   // MAX_NODES curved nodes spread over paths encodes under MAX_RECORD_BYTES
```

- [ ] **Step 2:** `cargo test -p umber-core vector::` — expect compile failure.
- [ ] **Step 3: Implement** the module with a private serde `Record` mirror (as `textobj` does), module docs stating: the role spellings and field names are a file format; why `Opaque` exists; why `print` is optional; that Ink is reserved and nothing renders it.
- [ ] **Step 4:** tests pass. Measure the largest encoded node in the last test, set `MAX_RECORD_BYTES` to `2 * (MAX_NODES * per_node + MAX_PATHS * per_path)` rounded up to a MiB, as a `const` expression with the measured figures named.
- [ ] **Step 5:** gates; commit `Add umber-core::vector, the paths a vector layer holds`.

### Task 2: The record on the layer and the stack

**Files:** Modify `crates/umber-core/src/layer.rs`.

**Interfaces — Consumes:** Task 1. **Produces:**

```rust
impl Layer { pub fn vector(&self) -> Option<&VectorObject>; pub fn is_vector(&self) -> bool; }
impl LayerStack {
    pub fn add_vector_layer(&mut self, name: &str, record: VectorObject) -> Option<usize>; // as add_named; None on add_refusal or failed check
    pub fn vector_at(&self, index: usize) -> Option<&VectorObject>;
    pub fn can_set_vector(&self, index: usize, record: &VectorObject) -> Result<(), VectorSetRefusal>;
    pub fn set_vector(&mut self, index: usize, record: VectorObject) -> Result<Option<Box<VectorObject>>, VectorSetRefusal>; // returns what was there
    pub fn restore_vector(&mut self, slot: u32, record: Option<Box<VectorObject>>) -> Result<Option<Box<VectorObject>>, Option<Box<VectorObject>>>; // undo's door: by slot, no lock gate; Err hands the record back when no layer holds the slot
    pub fn flip_vectors(&mut self, axis: FlipAxis, canvas: UVec2);
    pub fn translate_vectors(&mut self, by: Vec2) -> usize; // returns Ink-bearing records it stripped of Ink
    pub fn has_vector_layers(&self) -> bool;
}
pub enum VectorSetRefusal { Missing, NotVector, Locked, Record(VectorRefusal) }
pub enum EditRefusal { Folder, Locked, Text, Vector, Missing }   // ALL becomes [_; 5]
```

- `Layer::vector: Option<Box<VectorObject>>`, private; `None` in every constructor. `byte_len` for a parked layer adds the record's.
- `refusal_at` answers `Vector` for a layer with a record, after `Locked`, same position `Text` takes. `EditRefusal::Vector` sentence: "This is a vector layer. Paint on an ordinary layer instead."
- `set_vector` refuses `NotVector` on a layer whose `vector` is `None` (creation is `add_vector_layer`'s alone), `Locked` on `effective_locked`, `Record(Opaque)` when the existing record is opaque, and `Record(..)` from `check`. `set_text` refuses a vector layer.
- `translate_vectors`: Cut and Plot move; a record with Ink becomes `without_ink().translated(by)`.
- Layer restore / `Gone` already carries the whole `Layer`, so nothing is added there; a test proves it.

- [ ] **Step 1: Tests** (in `layer.rs` tests):

```rust
painting_on_a_vector_layer_is_refused_and_its_mask_is_not    // refusal_at == Some(Vector); the mask-stroke predicate the text test uses answers allowed
every_refusal_is_named_in_the_all_array                      // extend the existing exhaustive test
an_empty_vector_layer_is_a_vector_layer
a_plain_layer_cannot_be_given_paths                          // NotVector, stack unchanged (==)
a_locked_vector_layer_refuses_new_paths                      // own lock and a locked parent folder
a_refused_set_changes_nothing
deleting_a_vector_layer_and_undoing_brings_its_paths_back    // via remove_many + restore_shape
a_folder_deleted_with_a_vector_layer_in_it_comes_back_whole
a_flip_mirrors_every_vector_layer_and_twice_restores_it
a_resize_moves_cut_paths_and_strips_ink
restoring_by_slot_finds_the_layer_after_a_reorder            // Review Focus 3
a_text_layer_cannot_become_a_vector_layer_and_the_reverse
```

- [ ] **Step 2:** run, expect failure. **Step 3:** implement; update the module docs' text section with a "Vector layers" paragraph. **Step 4:** pass. **Step 5:** gates; fix every non-exhaustive match the new `EditRefusal` variant breaks in `umber-app` with the sentence above (a paste refusal: "This is a vector layer. Paste onto an ordinary layer instead."). Commit `Let a layer carry a vector record, and refuse paint on one`.

### Task 3: Undo

**Files:** Modify `crates/umber-core/src/history.rs`, `crates/umber-core/src/docformat/history.rs`, `crates/umber-core/src/docimport/history.rs`, `crates/umber-app/src/panels.rs` (`edit_icon`), `crates/umber-app/src/icons.rs` if a new mark is needed.

**Interfaces — Produces:**

```rust
EditKind::Paths                                   // label "Edit paths"; flip_axis None; is_structural false; resurrects_pixels false
EditBody::Vector { slot: u32, was: Option<Box<VectorObject>> }
impl EditKind { pub fn is_saved(self) -> bool; }  // exhaustive: false for the structural kinds and Paths, true otherwise
impl Edit { pub fn vector(slot: u32, was: Option<Box<VectorObject>>, at: Option<Timestamp>) -> Edit; } // match the existing constructors' shape
```

- `EditBody::Vector`'s budget cost is `was.byte_len()` plus the arm's size; it holds no patch (`patches()` is empty).
- `SaveHistory::new` skips an entry where `!kind.is_saved()`, replacing its `is_structural()` test; the reader's guard in `docimport::history` that is gated on `is_structural` uses the same predicate. `history::VERSION` does not move.
- `edit_icon(EditKind::Paths)` is `Icon::Spline` (Lucide `spline`; add the variant by pasting the package's `d` attributes, run `icon_sheet` and look for a collision). Task 6 reuses the icon.
- `kind_id`'s pinned literal list gains `"Paths"`.

- [ ] **Step 1: Tests:**

```rust
// history.rs
all_lists_every_edit_kind                         // extend the exhaustive guard
a_vector_entry_costs_its_record_and_is_evicted_like_any_other
// docformat/history.rs
a_paths_entry_is_not_written_and_does_not_cost_the_history   // history of [Paint, Paths, Paint] saves two entries and reloads two; assert the file holds no Flip body
every_unsaved_kind_is_skipped                     // for each kind with !is_saved(), a history containing one saves without it and reloads
```

- [ ] **Steps 2–4:** fail, implement, pass. Mutation: make `is_saved` answer `true` for `Paths` and confirm the second test fails.
- [ ] **Step 5:** gates; commit `Record a path edit as one undo entry that holds the record`.

### Task 4: The file

**Files:** Modify `crates/umber-core/src/docformat/mod.rs`, `crates/umber-core/src/docimport/openraster.rs`, `crates/umber-core/src/docimport/mod.rs`, `docs/group-compositing.md` (§4.3: group compositing takes revision 6), and the other docs CLAUDE.md names as claiming a revision if they name 5.

**Interfaces — Produces:**

```rust
// docformat
pub const VERSION: u32 = 5;
pub const VECTOR_VERSION: u32 = 5;
pub const VECTOR_ATTR: &str = "umber-vector";
pub struct SaveLayer<'a> { …, pub vector: Option<&'a VectorObject> }   // None in SaveLayer::new
pub enum SaveWarning { …, VectorNotRecorded { layer: String, why: VectorRefusal } }
// docimport
pub struct ImportedLayer { …, pub vector: Option<Box<VectorObject>> }
pub enum ImportWarning { …, VectorDropped { layer: String, reason: String }, VectorNeedsNewer { layer: String }, VectorInkDropped { layer: String } }
```

- Entry path `umber/vector/<n>.json`, `<n>` numbered the way `umber/text/` is. Written wherever `layer.vector` is `Some`, including an empty record.
- `required_version` answers `max(previous answer, 5)` when any layer has `vector.is_some()`, else exactly what it answered before.
- Writer: `to_json(Some(&print))` using the fingerprint `LayerImage` already computes; a `to_json` error writes no entry and pushes `VectorNotRecorded`.
- Reader, mirroring the text loader's shape with `read_optional_entry_bounded(…, vector::MAX_RECORD_BYTES)`: `Opaque` → keep, push `VectorNeedsNewer`; `Err` → `vector = None`, push `VectorDropped`; `Some(print)` not matching the layer image's fingerprint → `without_ink()`, push `VectorInkDropped`. A layer naming both `umber-text` and `umber-vector` keeps the text record and drops the vector one with `VectorDropped`. A folder naming one: dropped, same warning.
- `ImportedDocument::open` installs the record on the stack layer (the place `layer.text` is installed, `docimport/mod.rs` ~line 691).
- Warning sentences: `VectorNeedsNewer` "The paths on "{layer}" were made by a newer Umber. They are kept and saved back, but cannot be shown or changed here."; `VectorDropped` "The paths on "{layer}" could not be read and were left out. {reason}"; `VectorInkDropped` "Some strokes on "{layer}" were painted over elsewhere and are now ordinary paint."

- [ ] **Step 1: Tests** (docformat tests, using its existing fixture helpers):

```rust
a_vector_record_survives_a_save_and_a_reopen                 // Cut + Plot paths, equal after encode -> open
an_empty_vector_layer_is_still_one_after_reopening
a_document_with_a_vector_layer_declares_revision_5
a_document_without_one_declares_what_it_did_before           // run the existing version table unchanged
an_opaque_record_is_written_back_byte_for_byte               // hand-built archive with version 2 record -> open -> save -> entry bytes ==
an_unparseable_record_costs_the_layer_its_paths_only         // pixels intact, warning raised, layer.vector None
an_over_long_vector_entry_is_refused_unread                  // the text test's shape
ink_whose_pixels_changed_is_dropped_and_cut_is_kept          // hand-built record with Ink + Cut + wrong print
ink_whose_pixels_match_is_kept
a_layer_naming_text_and_vector_keeps_the_text
a_streamed_save_is_byte_for_byte_the_archive_encode_builds   // extend its fixture with a vector layer
```

- [ ] **Steps 2–4:** fail, implement, pass. Mutation: delete the writer's entry and confirm the round-trip test fails; make `required_version` ignore vector and confirm the revision test fails.
- [ ] **Step 5:** gates; commit `Write and read a vector layer's paths, and take umber-version to 5`.

### Task 5: Both savers, and the stack hooks in the application

**Files:** Modify `crates/umber-app/src/app.rs`, `autosave.rs`, `editor.rs`, `panels.rs`.

**Interfaces — Consumes:** Tasks 2–4. **Produces:** `Candidate`'s per-layer snapshot gains `vector: Option<Box<VectorObject>>`; `App`'s undo/redo applies `EditBody::Vector`.

- `app.rs` Save (~line 6489) sets `vector: layer.vector()`. `autosave.rs` snapshots it (~line 2488) and writes it (~line 2576).
- The literal-count guard for `text:` at `SaveLayer` construction sites gets a twin for `vector:` (same word-boundary rule; read that test's comment first).
- Undo/redo (`app.rs` ~line 966, beside `EditBody::Text`): `EditBody::Vector { slot, was }` calls `stack.restore_vector(slot, was)` and returns `EditBody::Vector { slot, was: previous }`; `Err` hands the body back as the existing arms do. No GPU work.
- Canvas flip (`mirror_document`): call `flip_vectors` where `flip_text` is called. Resize (`Editor::apply_canvas`): call `translate_vectors(offset)` with `CanvasCopy::plan`'s destination-minus-source offset, **not** dropping the records; a non-zero return raises the notice "Strokes on vector layers became ordinary paint, because the canvas changed size."
- `begin_float`, paste, cut: the existing `EditRefusal` routing now carries `Vector`; confirm each raises its sentence and starts nothing. A canvas press on a vector layer raises no dialog (the text layer's rule).
- "Clear layer": disabled where the active layer `is_vector()`, tooltip "A vector layer has no paint to clear. Delete the layer to remove its paths." `clear_active_layer` also refuses at its own gate.
- Import warnings from Task 4 are shown through whatever shows `TextDropped`.

- [ ] **Step 1: Tests:**

```rust
// app.rs
save_and_the_autosave_both_carry_the_vector_record           // the construction-site count guard
undoing_a_path_edit_puts_the_old_record_back_and_redo_the_new
a_canvas_flip_and_its_undo_leave_paths_where_they_were
a_resize_moves_paths_with_the_picture                        // anchor top-left grow by (100, 50) at centre anchor etc: path moved by the plan's offset
clear_layer_is_refused_on_a_vector_layer
// autosave.rs
an_autosave_writes_the_paths_a_save_does                     // Review Focus 4: mirror the text record's autosave round-trip test
```

- [ ] **Steps 2–4:** fail, implement, pass (`CI=1` is acceptable for the wall-clock test only). Mutation: delete the autosave's `vector:` line; the autosave test must fail.
- [ ] **Step 5:** gates; commit `Carry vector records through Save, the autosave, undo, flip and resize`.

### Task 6: Making a vector layer and a path from the selection

**Files:** Modify `crates/umber-app/src/shortcuts.rs`, `ui.rs` (`edit_menu`), `app.rs`, `editor.rs`, `panels.rs`, `widgets.rs` (layer row).

**Interfaces — Produces:**

```rust
Action::CutPathFromSelection      // label "Cut path from selection", id "CutPathFromSelection", unbound
Action::PlotPathFromSelection     // label "Plot path from selection", id "PlotPathFromSelection", unbound
Action::NewVectorLayer            // label "New vector layer", unbound
impl App { fn path_from_selection(&mut self, role: PathRole); fn add_vector_layer(&mut self) -> Result<MadeLayer, Notice>; }
```

- `path_from_selection`: finish any stroke and put down any float first (the rule every document-leaving path follows). No selection: nothing (the menu row is disabled). Build `VectorObject::from_rings(selection.rings(), role)`; empty result → notice with `VectorRefusal::Impossible`'s sentence. If the active layer is an unlocked, non-opaque vector layer: `appended` then `set_vector`, record `Edit::vector(slot, was, now)`. Otherwise: `add_refusal` first, then `add_vector_layer` above the active entry named "Cut lines" / "Plot lines", recorded as the existing `EditKind::AddLayer` structural entry (follow `add_text_layer`, `app.rs` ~line 2226, including `try_reserve`). Any refusal is a notice carrying that refusal's sentence, and nothing changes. Marks the document modified. The selection stays.
- Edit menu: both rows through `menu_item` directly under Deselect, `enabled` when a selection exists. `NewVectorLayer` goes on the Layers panel beside the existing add control as an `icon_button` with `Icon::Spline` and tooltip "New vector layer", subject to the header-width guard; if the guard's pinned width would move, put it on the flags row instead and say so.
- Layer row: where `layer.is_vector()` and the record has no Ink, draw `Icon::Spline` in the thumbnail's place (the folder mark's code path) and request no thumbnail.
- Each `Action` gets its arm in every exhaustive match over `Action` and its line in the pinned id lists.

- [ ] **Step 1: Tests:**

```rust
a_path_from_a_rectangle_selection_is_four_nodes_on_a_new_layer   // new layer above active, named "Cut lines", one AddLayer entry, undo removes the layer
a_second_path_lands_on_the_active_vector_layer                   // one Paths entry; undo leaves the first path
plot_makes_plot_paths
the_command_changes_nothing_when_refused                         // Review Focus 5: locked active vector layer -> new layer instead; stack at MAX -> notice, stack == before; folder selected -> new layer at top level beside it
the_menu_rows_are_dead_without_a_selection_and_still_name_themselves   // headless run_ui, read the galleys
a_vector_layers_row_asks_for_no_thumbnail
```

- [ ] **Steps 2–4:** fail, implement, pass. **Step 5:** gates; commit `Make a vector layer, and a cut or plot path from the selection`.

### Task 7: The lines on screen

**Files:** Modify `crates/umber-app/src/theme.rs` (palette), `ui.rs`; create `crates/umber-app/src/vectorview.rs` (the cache and the flatten-to-screen model, no painting).

**Interfaces — Produces:**

```rust
impl Palette { pub fn cut_line(&self) -> Color32; pub fn plot_line(&self) -> Color32; }
// vectorview.rs
pub struct VectorView { /* cache */ }
impl VectorView {
    pub fn lines<'a>(&'a mut self, doc: DocId, stack: &LayerStack, camera: &Camera, view: egui::Rect, ppp: f32) -> &'a [ScreenLine];
}
pub struct ScreenLine { pub role: PathRole, pub points: Range<usize> }   // into one shared Vec<Pos2>; expose it
fn ui::vector_paths(painter: &egui::Painter, ed: &mut Editor, view: egui::Rect);
```

- Colours are **derived, not tabled**: start from fixed hues (cut: red-magenta ~345°, plot: cyan-blue ~200°, in `Hsv`), then take lightness through `contrast` so each reads against `accent_underlay`'s opposite end; reuse `Palette::link_colour`'s separation test style. Guards: `the_machine_lines_read_over_any_artwork` (sweep grey and saturated artwork as `the_marquees_pair_reads_over_any_artwork` does: the better of underlay/top is >= 3:1 everywhere) and `cut_and_plot_are_told_apart_in_every_theme` (colour distance against each other, the accent and all six link colours at least the tightest shipped accent pair's figure, computed by the same function that test uses).
- Drawing: under-pass in `accent_underlay` at `ant_width(3.0)`, top pass in the role colour at `ant_width(1.5)`, solid `Shape::line`. Ink paths and opaque records draw nothing. Only layers with `effective_visible`. Drawn before the marquee and the transform box so both sit on top. Painter clipped to the canvas region.
- `VectorView` lives on `Editor` above the `--- documents ---` line, keyed by document id, a stack-level vector revision and the camera (position, zoom, rotation if any, pivot) and `view`; unchanged key returns the cached slice. The revision is a `u64` on `LayerStack` bumped inside every method that writes a `vector` field (Task 2's five writers plus add/remove/restore-shape and reorder); add `LayerStack::vector_revision()` and a test that each writer bumps it.
- Flatten at `0.25 / zoom / ppp` document px. **Culling (Review Focus 2):** skip a path whose `bounds()` mapped to screen misses `view` expanded by 8 points; clamp each emitted point to `view` expanded by 4096 points so no coordinate overflows; cap total points per frame at 262,144 and stop there (log once at `debug`).
- A stack with `!has_vector_layers()` returns before touching the cache.

- [ ] **Step 1: Tests:**

```rust
// vectorview.rs
a_visible_vector_layer_yields_its_lines_and_a_hidden_one_none      // incl. hidden by parent folder
an_unchanged_view_reuses_the_cache                                 // second call: no flatten (count via a test counter)
a_path_edit_or_a_camera_move_rebuilds_it
a_path_off_screen_is_culled_and_a_huge_one_is_clamped              // at zoom 64 a path 1e6 px away yields nothing; one crossing the view yields finite points within the clamp
// ui.rs, headless
the_overlay_draws_a_cut_line_in_the_cut_colour                     // frame_pixel or inks_drawn on a known pixel of a horizontal path
a_document_export_ignores_cut_paths                                // umber-app gputest: export_rgba with and without the record is byte-identical
// theme.rs: the two palette guards above
```

- [ ] **Steps 2–4:** fail, implement, pass. Mutation: drop the `effective_visible` filter; the first test must fail.
- [ ] **Step 5:** gates; add a `docshot` shot of a canvas with a cut line if `docshot::Stage` can stage a selection-made path without a GPU, else skip and say so. Commit `Draw cut and plot paths over the canvas`.

### Task 8: Prose

**Files:** `CHANGELOG.md`, `README.md`, `CLAUDE.md`, `docs/architecture.md` roadmap.

- [ ] CHANGELOG, under the open version: vector layers; the two commands; the stated limit (a combined selection gives a stair-stepped path until smoothing arrives); documents with vector layers need this version or newer.
- [ ] README: two or three sentences under the features, and "What is not there yet": drawing and editing paths, tracing artwork, sending to a machine.
- [ ] CLAUDE.md: a "Vector layers" subsection under Layers with the rules that would be re-derived wrongly: record-is-the-kind (empty is a record); Cut/Plot never rasterise and Ink is reserved; `Opaque` is kept so a save cannot delete a newer build's paths; `print` only beside Ink; version 5 and group compositing now 6; `EditKind::is_saved` replaced the `is_structural` skip; `vector_revision` is bumped inside the writers; resize translates where text drops. Plus every implementer's reported prose.
- [ ] Run the whole suite with no `CI=1` on a quiet machine; commit `Document vector layers`.
