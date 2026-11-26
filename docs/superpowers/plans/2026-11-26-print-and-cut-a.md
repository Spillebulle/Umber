# Print and cut, plan A: everything that needs no machine

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Registration marks on a document, a print sheet PDF, the encoder that turns Cut and Plot paths into Silhouette commands, the conversation with the machine tested against a fake, and a probe the artist runs on the real Portrait 3.

**Architecture:** Three pure modules in `umber-core` (`sheet`, `printsheet`, `cutjob`), each tested with no device. `umber-app::cutter` holds a `Link` trait, the `send` conversation over it, and a Windows `UsbPrint` link. No dialog and no worker thread in this plan: those are plan B, after the probe has been run.

**Tech Stack:** Rust, glam, flate2 (already a dependency of `umber-core`), egui for the overlay, windows-sys 0.61 for the link.

**Spec:** `docs/superpowers/specs/2026-11-26-print-and-cut-design.md`

## Global Constraints

- `umber-core` must not depend on wgpu, winit, egui or windows-sys.
- Every `match` over an enum is exhaustive; no `matches!` where a `match` would fail the build.
- No hard-coded colours in `umber-app`: everything from `theme::Palette` or `contrast::ink_on`. (The PDF's black marks and white matte are file content, not interface colours.)
- No Unicode symbols in interface strings. British spelling. No em-dashes in user-facing text.
- A strip of buttons is `controls::button_row` inside a `horizontal`.
- Tests measure what was produced (bytes written, shapes drawn), never restate the rule.
- One commit per coherent change. `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings` and the tests of the touched crates pass before a task is reported done.
- `CARGO_TARGET_DIR` is `<worktree>/target`, set on every cargo invocation.
- Do not edit `CLAUDE.md`, `README.md` or `CHANGELOG.md` before Task 6; report prose for them instead.
- Do not drive a GUI or run the app. Do not open any USB device in a test.
- Machine units: 20 per millimetre. A point is sent as `y,x`. Every command ends in ETX (`0x03`).
- Constants, verbatim: `INSET_MM = 10.0`, `SQUARE_MM = 5.0`, `ARM_MM = 20.0`, `STROKE_MM = 0.5`, `MIN_SPAN_MM = 60.0`, `MAX_CUT_WIDTH_MM = 203.0`, `FLATTEN_MM = 0.05`, `MAX_POINTS = 200_000`, `MAX_LENGTH_MM = 18290.0`.

## Review Focus

- A document whose dpi makes the page a fraction of a millimetre under a threshold: `for_page` must use the physical size, not a rounded one, and the same answer must reach the overlay, the PDF and the job.
- A transparent document: the PDF must matte onto white in linear light, never black.
- A reply from the machine that is not understood (`3` ETX, an empty read, a reply with different spacing): never read as ready or as marks found.
- A path with one node, or a closed path of two: flattened to something the encoder does not turn into a stray `M` with no `D`, or a cut of zero length.
- A canvas resized or re-created while marks are on: the flag must not survive onto a page that cannot hold marks, and must not leak into File > New.

---

### Task 1: The marks model

**Files:**
- Create: `crates/umber-core/src/sheet.rs`
- Modify: `crates/umber-core/src/lib.rs` (`pub mod sheet;`)

**Interfaces:**
- Produces:
  ```rust
  pub const INSET_MM: f32 = 10.0; pub const SQUARE_MM: f32 = 5.0; pub const ARM_MM: f32 = 20.0;
  pub const STROKE_MM: f32 = 0.5; pub const MIN_SPAN_MM: f32 = 60.0; pub const MAX_CUT_WIDTH_MM: f32 = 203.0;
  /// A rectangle in millimetres from the page's top left.
  #[derive(Clone, Copy, Debug, PartialEq)] pub struct RectMm { pub min: Vec2, pub max: Vec2 }
  #[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum NoMarks { TooSmall, TooWide }
  impl NoMarks { pub fn sentence(self) -> &'static str }
  #[derive(Clone, Copy, Debug, PartialEq)] pub struct Marks { pub page: Vec2, pub span: Vec2 }
  impl Marks {
      pub fn for_page(width_mm: f32, height_mm: f32) -> Result<Marks, NoMarks>;
      pub fn for_document(doc: &Document) -> Result<Marks, NoMarks>;   // mm = px * 25.4 / dpi
      pub fn origin(&self) -> Vec2;            // (INSET_MM, INSET_MM)
      pub fn cut_area(&self) -> RectMm;        // origin .. origin + span
      pub fn fills(&self) -> [RectMm; 5];      // the square, then two arms per L, as filled rectangles
      pub fn keep_clear(&self) -> [RectMm; 3]; // 20 mm squares at the three marked corners, inside the span
  }
  pub fn mm_per_pixel(dpi: f32) -> f32;         // 25.4 / sane dpi
  ```
  (The spec says `Option`; `Result<_, NoMarks>` is this plan's refinement so a refusal can say why.)
- An L's arms are rectangles `ARM_MM` long and `STROKE_MM` wide whose outer edges meet at the corner point and which lie inside the span rectangle: top right corner at `(page.x - INSET, INSET)` with arms running left and down; bottom left at `(INSET, page.y - INSET)` with arms running right and up.

- [ ] **Step 1: Write failing tests** in `sheet.rs`:
  - `a4_marks_are_where_the_machine_is_told_they_are`: `for_page(210.0, 297.0)` gives `span == (190, 277)`, the square is `(10,10)..(15,15)`, and every fill lies inside `cut_area()`.
  - `each_l_is_two_arms_meeting_at_its_corner`: for A4 the top right arms both touch `(200, 10)` and the bottom left both touch `(10, 287)`; each is 20 by 0.5.
  - `a_page_too_small_or_too_wide_has_no_marks`: `for_page(79.9, 200.0)` is `Err(TooSmall)`; `for_page(80.0, 80.0)` is `Ok`; `for_page(223.1, 297.0)` is `Err(TooWide)`; `for_page(223.0, 297.0)` is `Ok`. NaN and negative sizes are `Err(TooSmall)`.
  - `a_document_is_measured_by_its_resolution`: a 2480 x 3508 document at 300 dpi gives spans within 0.05 mm of `(189.97, 277.01)`.
  - `keep_clear_covers_every_mark`: each fill lies inside one of `keep_clear()`.
  - `both_refusals_say_something_different_and_carry_no_dash`: sentences differ, are non-empty, contain no `\u{2014}`.
- [ ] **Step 2: Run** `cargo test -p umber-core sheet` and see them fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the tests, fmt, clippy.
- [ ] **Step 5: Commit.**

### Task 2: The flag on the document, saved and loaded

**Files:**
- Modify: `crates/umber-core/src/document.rs` (field `pub marks: bool`, `with_marks(self, bool) -> Self`)
- Modify: `crates/umber-core/src/docformat/mod.rs` (`SaveDocument::marks: bool`, `pub const MARKS_ATTR: &str = "umber-regmarks"`, written on `<image>` as `umber-regmarks="1"` only when true; `required_version` untouched)
- Modify: `crates/umber-core/src/docimport/mod.rs` (`ImportedDocument::marks: bool`, carried by `document()`), `openraster.rs` (read it), and every other `ImportedDocument` literal (`false`)
- Modify: `crates/umber-app/src/app.rs:4069`, `autosave.rs` (`Candidate::marks`, `snapshot`, `run_task`), `canvasdlg.rs` (`CanvasForm::document()`)
- Modify: `crates/umber-app/src/editor.rs` (`apply_canvas`)

**Interfaces:**
- Consumes: `sheet::Marks::for_document`.
- Produces: `Document::marks`, `Document::with_marks`, `SaveDocument::marks`, `ImportedDocument::marks`, `docformat::MARKS_ATTR`.
- Rules: Canvas settings (Apply) carries the flag from the document it was seeded from; File > New always creates with `marks: false`. `Editor::apply_canvas` clears the flag and adds a notice (`title: "Registration marks removed"`, line: "The new canvas size cannot hold registration marks, so they were switched off.") when the new document cannot hold marks. A flip leaves it alone. A resize that still admits marks keeps it.

- [ ] **Step 1: Write failing tests:**
  - core `docformat`: `registration_marks_survive_a_save_and_a_reopen` (encode with `marks: true`, read back, `document().marks`); `a_document_without_marks_writes_no_attribute` (the stack.xml text does not contain `umber-regmarks`); `marks_do_not_raise_the_version` (a one-layer document with marks declares `umber-version="1"`).
  - app `autosave`: `an_autosave_keeps_the_registration_marks` in the style of the existing test that pins `effects`/`text` reaching the autosaved file; and extend the existing guard that counts `SaveDocument` literals if one exists, otherwise assert both writers through their outputs.
  - app `canvasdlg`: `canvas_settings_keeps_the_marks_and_new_does_not`.
  - app `editor`: `a_resize_too_small_for_marks_switches_them_off_and_says_so`; `a_resize_that_still_fits_keeps_the_marks`.
- [ ] **Step 2: Run** and see them fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p umber-core` and `cargo test -p umber-app`, fmt, clippy.
- [ ] **Step 5: Commit.**

### Task 3: The command and the overlay

**Files:**
- Modify: `crates/umber-app/src/shortcuts.rs` (`Action::ToggleSheetMarks`, label "Registration marks", category "File", unbound; `ALL` grows by one; guard tests updated)
- Modify: `crates/umber-app/src/app.rs` (`handle_keys` arm), `editor.rs` (`pub fn toggle_sheet_marks(&mut self)`), `ui.rs` (File menu row with a tick; `fn sheet_marks(painter, p, ed, view)` called immediately before `vector_paths`)

**Interfaces:**
- Consumes: `Document::marks`, `sheet::Marks`, `vectorview::screen_mapping`.
- Produces: `Editor::toggle_sheet_marks`. Turning on where `for_document` is `Err(reason)` leaves the flag off and adds a notice (`title: "No room for registration marks"`, line: `reason.sentence()`). Toggling marks the tab modified.
- Overlay: the five fills in `contrast::ink_on(p.backdrop.., Ink::Strong)`-style ink chosen against white paper is wrong for a dark document, so draw each fill as a two-tone mark: `p.accent_underlay()` outline one device pixel wide round a fill of `p.accent`. Keep-clear zones: an outline in `p.accent` at reduced alpha via the palette's existing dim accent (`p.accent_dim`), no fill. Nothing is drawn when `doc.marks` is false.

- [ ] **Step 1: Write failing tests:**
  - `shortcuts`: `the_marks_action_is_listed_unbound_under_file`; update the exhaustive index guard.
  - `editor`: `toggling_marks_on_a_page_too_small_refuses_and_says_why`; `toggling_marks_marks_the_document_modified`.
  - `ui` (headless `run_ui`, reading shapes): `registration_marks_are_drawn_only_when_the_document_has_them` (count of shapes inside the canvas rect rises by at least five and falls back); `the_marks_overlay_sits_where_the_model_puts_them` (the square's screen rectangle equals `screen_mapping` of the model's first fill converted to pixels, within half a point).
  - `ui`: the existing `every_file_edit_and_view_command_has_a_menu_row` must pass with the new action.
- [ ] **Step 2: Run** and see them fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p umber-app`, fmt, clippy.
- [ ] **Step 5: Commit.**

### Task 4: The print sheet

**Files:**
- Create: `crates/umber-core/src/printsheet.rs`
- Modify: `crates/umber-core/src/lib.rs`; `crates/umber-app/src/shortcuts.rs` (`Action::ExportPrintSheet`, "Export print sheet…", category "File", unbound); `app.rs` (`fn export_print_sheet(&mut self)`, `handle_keys` arm, spent from a `UiActions::export_print_sheet: bool`); `ui.rs` (File menu row under Export)

**Interfaces:**
- Consumes: `sheet::Marks`, `CanvasRenderer::export_rgba`, `docformat::write_with`, `rfd::FileDialog`.
- Produces:
  ```rust
  pub struct Sheet<'a> { pub size: UVec2, pub dpi: f32, pub rgba: &'a [u8], pub marks: Option<Marks> }
  #[derive(Debug)] pub enum SheetError { WrongSize { found: usize, expected: usize }, Io(std::io::Error) }
  pub fn write(out: &mut impl std::io::Write, sheet: &Sheet<'_>) -> Result<(), SheetError>;
  pub fn default_file_name(title: &str) -> String;   // "<title> print sheet.pdf"
  ```
- The PDF: version 1.4; one page; MediaBox `[0 0 w h]` in points to three decimals, `w = px * 72 / dpi`; one image XObject, `/DeviceRGB`, 8 bits, `/FlateDecode`, drawn with `cm` filling the page; `rgba` is straight-alpha sRGB as `export_rgba` returns, matted onto white **in linear light** (decode, mix, re-encode; reuse `docimport::srgb` or `Color`'s existing conversions, no second `powf` table if one exists); marks as `re ... f` fills in `0 g`, with y flipped (PDF's origin is bottom left); a correct `xref` table with byte offsets and a trailer. Rows are streamed through the encoder: do not build a second canvas-sized RGB buffer beyond one row at a time.
- App: the command finishes a stroke and a float as `App::export` does, uses `baked_draws(None)`, writes through `write_with`, and on success raises a notice (`title: "Print sheet exported"`, lines: the path, and "Print it at actual size (100%), with no fit to page, or the cuts will not line up."). Where the document has no marks the notice adds: "This document has no registration marks. Switch them on in the File menu to print and cut."

- [ ] **Step 1: Write failing tests** in `printsheet.rs`:
  - `the_page_is_the_documents_physical_size`: 2480 x 3508 at 300 dpi gives a MediaBox within 0.01 pt of `595.2 x 841.92`.
  - `the_cross_reference_table_points_at_every_object`: parse `startxref`, read each offset, and assert the bytes there begin `"<n> 0 obj"`.
  - `the_picture_survives_the_round_trip`: inflate the image stream of a 3 x 2 sheet of known opaque colours and compare RGB byte for byte.
  - `transparency_mattes_onto_white_in_linear_light`: alpha 0 gives `255,255,255`; black at alpha 128 gives `188 +/- 1` per channel, not 127.
  - `marks_are_drawn_where_the_model_puts_them`: for A4 with marks, the content stream holds five `re` operators, and the first is the square at x 28.346, y `841.89 - 42.52`, 14.173 square, each within 0.01.
  - `a_sheet_without_marks_draws_none`: no `re` in the content stream.
  - `a_buffer_of_the_wrong_length_is_refused`.
  - app `shortcuts`/`ui`: the action is listed under File and has a menu row.
- [ ] **Step 2: Run** and see them fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p umber-core printsheet`, `cargo test -p umber-app`, fmt, clippy.
- [ ] **Step 5: Commit.**

### Task 5: The cut job encoder

**Files:**
- Create: `crates/umber-core/src/cutjob.rs`
- Modify: `crates/umber-core/src/lib.rs`

**Interfaces:**
- Consumes: `vector::{Path, PathRole}`, `Path::flatten`, `trace::ring_area`, `sheet::{Marks, mm_per_pixel, MAX_CUT_WIDTH_MM}`, `LayerStack`.
- Produces:
  ```rust
  pub const UNITS_PER_MM: f32 = 20.0; pub const ETX: u8 = 0x03;
  pub const FLATTEN_MM: f32 = 0.05; pub const MAX_POINTS: usize = 200_000; pub const MAX_LENGTH_MM: f32 = 18290.0;
  pub const BLOCK_BYTES: usize = 1024;
  #[derive(Clone, Copy, Debug, PartialEq, Eq)]
  pub struct JobSettings { pub role: PathRole, pub use_marks: bool, pub pressure: u8, pub speed: u8, pub passes: u8 }
  impl JobSettings { pub fn new(role: PathRole) -> Self }  // Cut 10/5/1, Plot 5/8/1, marks on; Ink as Plot
  #[derive(Clone, Debug, PartialEq)]
  pub enum JobRefusal { NoPaths(PathRole), InkRole, OutsideCutArea, OutsidePage, PageTooWide, NoMarks, TooManyPoints }
  impl JobRefusal { pub fn sentence(&self) -> String }
  pub struct Page { pub size_mm: Vec2, pub mm_per_pixel: f32, pub marks: Option<Marks> }
  impl Page { pub fn of(doc: &Document) -> Page }          // marks only where doc.marks and for_document is Ok
  pub fn paths_of(stack: &LayerStack, role: PathRole) -> Vec<&Path>;  // visible vector layers, stack order
  pub struct Job { /* private */ }
  pub fn plan(paths: &[&Path], settings: &JobSettings, page: &Page) -> Result<Job, JobRefusal>;
  impl Job {
      pub fn setup_block(&self) -> Vec<u8>;
      pub fn marks_block(&self) -> Option<Vec<u8>>;   // ends with the TB123 command
      pub fn path_blocks(&self) -> Vec<Vec<u8>>;      // each at most BLOCK_BYTES, split at ETX
      pub fn end_block(&self) -> Vec<u8>;
      pub fn path_count(&self) -> usize; pub fn length_mm(&self) -> f32;
  }
  ```
- Settings are clamped: pressure `1..=33`, speed `1..=10`, passes `1..=5`.
- Bytes, with `<n>` decimal integers and `·` standing for ETX:
  - setup: `TG0·FN0·TB50,0·\0,0·Z<len>,<wid>·FX<pressure>,1·!<speed>,1·FC18,1,1·FE0,1·` for Cut (`FC0,1,1` for Plot), where `len = round(min(page height mm, MAX_LENGTH_MM) * 20)` and `wid = round(min(page width mm, 203) * 20)`.
  - marks: `TB99·TB52,2·TB51,400·TB53,10·TB55,1·TB123,<span_y*20>,<span_x*20>,0,0·`
  - a path: `M<y>,<x>·` then `D<y>,<x>·` for every further point, the whole path repeated `passes` times.
  - end: `M0,0·`
- A flattened path with fewer than two distinct rounded points is dropped; if that leaves none the refusal is `NoPaths`. Consecutive points that round to the same machine unit are merged.
- Closed paths sorted by ascending absolute `ring_area` of their flattening, then open paths in the order given.
- `PageTooWide` is refused without marks when the page is wider than 203 mm **and** a path reaches past 203 mm; with marks the page admitted marks, so the cut area already fits.

- [ ] **Step 1: Write failing tests:**
  - `a_square_becomes_these_exact_bytes`: a closed Cut square with corners at 20 mm and 30 mm on an A4 page with marks, default settings; assert `setup_block`, `marks_block` and `path_blocks` against literal byte strings (path: `M200,200·D200,400·D400,400·D400,200·D200,200·`; marks: `...TB123,5540,3800,0,0·`; setup for A4: `...Z5940,4060·FX10,1·!5,1·FC18,1,1·FE0,1·`).
  - `without_marks_coordinates_are_from_the_page_corner`: same square gives `M400,400·...`, and `marks_block()` is `None`.
  - `a_plot_job_has_no_blade_offset`: setup holds `FC0,1,1` and not `FC18`.
  - `a_point_is_sent_y_first`: a line from (10 mm, 30 mm) to (50 mm, 30 mm) without marks is `M600,200·D600,1000·`.
  - `passes_repeat_each_path_whole`.
  - `small_closed_paths_are_cut_before_large_ones`.
  - `a_curve_is_flattened_within_a_twentieth_of_a_millimetre`: a circle of radius 30 mm from four cubic nodes; every emitted point is within 0.1 mm of the true circle and there are more than 16.
  - `blocks_never_split_a_command_and_never_exceed_the_limit`: a long path; every block ends in ETX and is at most 1024 bytes; concatenated they equal the unsplit stream.
  - `a_path_outside_the_cut_area_refuses_the_whole_job`; `...outside the page...` without marks; `marks_asked_for_on_a_document_without_them_is_refused`; `no_paths_of_the_role_is_refused` (and paths of the other role alone still refuse); `a_degenerate_path_is_dropped_and_alone_refuses`; `too_many_points_is_refused`.
  - `settings_are_clamped_into_what_the_machine_takes`.
  - `every_refusal_has_its_own_sentence` over an exhaustive list.
  - `paths_of_skips_hidden_layers_and_the_other_role`.
- [ ] **Step 2: Run** and see them fail.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p umber-core cutjob`, fmt, clippy.
- [ ] **Step 5: Commit.**

### Task 6: The link, the conversation, the probe, and the docs

**Files:**
- Create: `crates/umber-app/src/cutter.rs`, `crates/umber-app/examples/probe-cutter.rs`
- Modify: `crates/umber-app/src/lib.rs` (`pub mod cutter;`), `crates/umber-app/Cargo.toml` (windows-sys features `Win32_Devices_DeviceAndDriverInstallation`, `Win32_Storage_FileSystem`, `Win32_System_IO`, `Win32_Security` as needed)
- Modify: `CLAUDE.md` (a "#### Print and cut" section after "#### Trace outline"), `README.md`, `CHANGELOG.md` (the 0.1.7 section)

**Interfaces:**
- Consumes: `cutjob::Job`.
- Produces:
  ```rust
  pub const VENDOR: u16 = 0x0b4d; pub const PORTRAIT_3: u16 = 0x113a;
  #[derive(Debug)] pub enum LinkError { NotFound, Busy, Timeout, Io(String), Unsupported }
  pub trait Link { fn write(&mut self, bytes: &[u8]) -> Result<(), LinkError>;
                   fn read_reply(&mut self, timeout: Duration) -> Result<Vec<u8>, LinkError>; }
  #[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Status { Ready, Moving, NoMedia }
  pub fn status_of(reply: &[u8]) -> Result<Status, Vec<u8>>;   // exactly b"0\x03", b"1\x03", b"2\x03"
  pub fn marks_found(reply: &[u8]) -> bool;                    // exactly b"    0\x03"
  #[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Stage { Connecting, FindingMarks, Cutting { block: usize, of: usize }, Finishing }
  #[derive(Debug)] pub enum SendError { Link(LinkError), NoMedia, MarksNotFound(Vec<u8>), Unknown(Vec<u8>), NeverReady }
  impl SendError { pub fn sentence(&self) -> String }
  pub struct Timing { pub poll: Duration, pub ready: Duration, pub marks: Duration, pub reply: Duration }
  impl Timing { pub const REAL: Timing }   // 250 ms, 120 s, 40 s, 5 s
  pub fn send(link: &mut dyn Link, job: &Job, timing: &Timing, progress: &mut dyn FnMut(Stage)) -> Result<String, SendError>; // Ok is the firmware string
  pub fn probe(link: &mut dyn Link, timing: &Timing) -> Result<(String, Result<Status, Vec<u8>>), LinkError>;
  pub struct UsbPrint { /* Windows handle */ }
  impl UsbPrint { pub fn open(product: u16) -> Result<UsbPrint, LinkError>; pub fn paths() -> Vec<String>; }
  ```
- The conversation: write `\x1b\x04`; write `FG\x03`, read the firmware reply (bytes before ETX, as lossy text, trimmed); status is `\x1b\x05` then one reply. Before the setup block, poll until `Ready`; `NoMedia` there is `SendError::NoMedia`. Write the setup block, wait ready. With a marks block: write it, one `read_reply(timing.marks)`; anything but `marks_found` is `MarksNotFound(raw)` and **no path block is written**. Then each path block followed by wait-ready, then the end block. `Moving` keeps polling until `timing.ready` has passed (`NeverReady`). An unrecognised status is `Unknown(raw)` at once. **Any `LinkError` from a write ends the job; nothing is ever written twice.**
- `UsbPrint` on Windows: `SetupDiGetClassDevsW` over the interface GUID `{28d78fad-5a12-11d1-ae5b-0000f803a8c2}` with `DIGCF_PRESENT | DIGCF_DEVICEINTERFACE`, `SetupDiEnumDeviceInterfaces`, `SetupDiGetDeviceInterfaceDetailW`; keep paths containing `vid_0b4d` (case-insensitive) and, for `open`, `pid_113a`. `CreateFileW(GENERIC_READ | GENERIC_WRITE, share 0, OPEN_EXISTING, FILE_FLAG_OVERLAPPED)`. `ERROR_SHARING_VIOLATION` and `ERROR_ACCESS_DENIED` are `Busy`. Reads are overlapped with a timeout and `CancelIoEx` plus `GetOverlappedResult(wait = TRUE)` on expiry, accumulating into a buffer split at ETX with trailing bytes kept for the next reply. Writes are overlapped and waited to completion; a short or failed write is `Io`. Every `unsafe` block carries a `// SAFETY:` comment. Off Windows `open` answers `Unsupported` and `paths` is empty. The path-matching rule is a pure function `fn is_cutter(path: &str, product: Option<u16>) -> bool` with tests.
- `probe-cutter`: prints the paths found; opens the Portrait 3; runs `probe`, wrapping the link so every byte written and read is printed in hex and as escaped ASCII; prints the firmware string and the status in words; sends nothing else. Its `//!` header says what it does, that it moves nothing, that Silhouette Studio must be closed, and the command: `cargo run -p umber-app --example probe-cutter`. Exit code is failure when the machine is not found or busy, with a sentence saying which.
- Docs: `CLAUDE.md` section records the rules that are easy to break: one function for the marks read by overlay, PDF and job; nothing is resent; an unknown reply is never ready; the spec's "what the first tests on the machine have to settle" list verbatim as **not yet verified on hardware**; nobody has run `UsbPrint`. `README.md`: one short paragraph under the vector features and an honest line under "What is not there yet" (sending to the machine has no command yet). `CHANGELOG.md`: one bullet in 0.1.7.

- [ ] **Step 1: Write failing tests** in `cutter.rs` with a scripted `FakeLink` recording every write and replaying queued replies:
  - `a_job_with_marks_is_written_in_order_and_once`: the concatenated writes equal `ESC EOT`, `FG·`, status queries, setup, marks, path blocks, end; no block appears twice.
  - `marks_not_found_ends_the_job_before_any_cut`: reply `"    1\x03"`; no write after the marks block contains `D`.
  - `a_marks_reply_with_other_spacing_is_not_found`: `b"0\x03"` and `b"   0\x03"` are both failures.
  - `no_media_is_said_before_anything_is_set_up`.
  - `an_unknown_status_is_reported_raw_and_never_ready`: `b"3\x03"`, `b"4\x03"`, `b"\x03"`, empty.
  - `moving_keeps_polling_and_then_gives_up` (with a `Timing` of milliseconds).
  - `a_failed_write_is_never_retried`: the fake fails the second path block; exactly one attempt at it is recorded and nothing follows.
  - `a_job_without_marks_sends_no_search`.
  - `progress_reports_each_stage_in_order`.
  - `every_send_error_has_its_own_sentence_and_no_dash` (exhaustive).
  - `the_cutter_is_told_from_a_printer_by_its_ids` for `is_cutter`.
  - `probe_sends_only_the_three_harmless_commands`: the writes are exactly `\x1b\x04`, `FG\x03`, `\x1b\x05`.
- [ ] **Step 2: Run** and see them fail.
- [ ] **Step 3: Implement** `cutter.rs`, then the example.
- [ ] **Step 4: Run** `cargo test -p umber-app cutter`, `cargo build -p umber-app --example probe-cutter`, the whole workspace's tests, fmt, clippy.
- [ ] **Step 5: Write the docs. Commit** code and docs separately.
