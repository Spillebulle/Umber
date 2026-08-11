# Audit: ways Umber can die, or lose the device

Read against `main` at `cd44fa1`. The remit is process death and unusability —
not silent wrongness, not performance.

**The headline: there is one BLOCKING finding, and it is not a panic.** It is a
wedge. A panic on the import worker leaves Umber showing a modal with no Cancel,
on top of the quit prompt, with the window close refused. Every other route out
of this codebase that I could reach is either an abort from an unbounded
allocation on a hostile file, or a deliberate uncaptured-device-error panic that
`CLAUDE.md` argues for and I am not disputing.

Panics are genuinely rare here. Forty-four `unwrap`/`expect`/`panic!` sites exist
outside tests across 170 files, and every one I read is either constant-derived,
preceded by the check it names, or in a fixture builder. `sqlite.rs`,
`csblocks.rs`, `lzf.rs` and `container.rs` are hardened against hostile input to
a standard I could not break by reading. The residual risk has moved out of the
parsers and into **allocation** and into **what happens when a worker dies**.

## Ranked

| # | Rank | Finding | Where | Evidence |
|---|---|---|---|---|
| 1 | **BLOCKING** | A dead import worker wedges the application permanently: no Cancel, quit prompt unreachable, window close refused | `loading.rs:150`, `tabs.rs:676`, `ui.rs:323`/`358`, `app.rs:5387` | confirmed by reading |
| 2 | SUBSTANTIVE | A PNG header alone drives an unbounded zeroed allocation, before any dimension check — aborts, so no crash report and no autosave | `flat.rs:42–45`, `preview.rs:252–255` | confirmed by reading (incl. `png` 0.18.1 source) |
| 3 | SUBSTANTIVE | ZIP entries whose cardinality the format does not bound are read at the 16 GiB *document* ceiling | `history.rs:116`, `openraster.rs:155`, `krita.rs:120` | confirmed by reading |
| 4 | SUBSTANTIVE | `preview::psd_composite` is the one PSD entry point with no `catch_unwind`, and the crate is documented to panic on real files | `preview.rs:226–232` vs `photoshop.rs:296` | confirmed by reading |
| 5 | SUBSTANTIVE | `try_reserve`'s enumeration of what is still fatal is stale in **both** directions | `canvas.rs:805–831` vs `canvas.rs:6542`, `canvas.rs:4826` | confirmed by reading |
| 6 | SUBSTANTIVE | `panic = "abort"` is **not** set, contrary to `CLAUDE.md`; `umber-shellext::guard` is load-bearing only because it is not | no `Cargo.toml` sets it; `umber-shellext/src/lib.rs:98` | confirmed by reading |
| 7 | MINOR | `Loading::start` uses `thread::spawn`, which panics on the main thread if the OS refuses a thread | `loading.rs:93` | confirmed |
| 8 | MINOR | `import_reporting` reads the whole file with no bound; the shell extension bounds the same bytes at 1 GiB | `docimport/mod.rs:144` vs `umber-shellext/src/lib.rs:208` | confirmed |
| 9 | MINOR | Unchecked `i64` addition of a tile offset in the Krita reader | `krita.rs:812` | confirmed; debug-only |
| 10 | MINOR | `Preview::fit_within`'s unreachable arm returns a `Preview` that violates `Preview::new`'s own invariant | `preview.rs:100–108` | confirmed |

Counts: **1 blocking, 5 substantive, 4 minor.**

The single thing I would most want fixed is **#1**, and it is one line.

---

## 1. BLOCKING — a dead import worker wedges Umber with no way out

### The mechanism

`Loading::take` (`crates/umber-app/src/loading.rs:150`):

```rust
pub fn take(&self) -> Option<Result<ImportedDocument, ImportError>> {
    self.outcome.try_recv().ok()
}
```

`.ok()` collapses `TryRecvError::Disconnected` into the same `None` that
`TryRecvError::Empty` produces. A worker that dropped its `Sender` without
sending — which is exactly what a panic on that thread does — is therefore
indistinguishable from a worker that is still decoding. `Editor::loading` is
never cleared, and `App::collect_loading` (`app.rs:4498`) returns early for ever.

Three things then compound, and each is individually defensible:

1. **The modal has no Cancel, deliberately.** `tabs::loading` (`tabs.rs:666–676`)
   says so: *"Modal, and deliberately without a Cancel. The decode runs on a
   worker that owns its own buffers and cannot be interrupted part way… Stopping
   is not offered rather than offered and ignored."* Sound while the worker is
   alive; there is nothing to stop when it is dead.

2. **It is the topmost modal, so the quit prompt beneath it is inert.**
   `ui::draw` shows `tabs::quit_prompt` at `ui.rs:323` and `tabs::loading` at
   `ui.rs:358`. egui 0.35's `Modal` documents *"The topmost modal will always be
   the most recently shown one"*, and `Modal::show` calls
   `mem.set_modal_layer(area.layer())`, which blocks interaction below it. So the
   quit prompt is drawn, dimmed by the loading modal's own backdrop, and cannot
   be clicked; its `should_close` reads `is_top_modal`, which is false.

3. **The window refuses to close.** `app.rs:5387`:
   ```rust
   WindowEvent::CloseRequested => {
       if self.editor.unsaved_documents().is_empty() { event_loop.exit(); }
       else { self.editor.ui.quit_prompt = true; ... }
   }
   ```
   With any unsaved document open, the close is converted into the prompt that
   cannot be reached.

The artist is left with a progress bar that never advances, no Cancel, no
reachable quit, and every open document unsaved. The only exit is killing the
process, which is exactly the loss the close refusal exists to prevent.

**And nothing is said.** `crash::report_panic` (`crash/mod.rs:346–352`) returns
early for a thread that is not `main` — correctly, per `CLAUDE.md` — so the
outcome is one `log::error!` line and a modal that never moves.

### Reachability, stated honestly

The *mechanism* is confirmed by reading. The *trigger* I did not demonstrate: I
read `sqlite.rs`, `csblocks.rs`, `lzf.rs`, `container.rs`, `flat.rs`,
`openraster.rs`'s `parse_stack`/`load_layer`, `krita.rs`'s tile path and
`docimport/history.rs` looking for a reachable panic on the worker and did not
find one. `photoshop::read` wraps the panicking crate in `catch` itself
(`photoshop.rs:296`). The ORA parser is iterative, not recursive, so a deeply
nested `stack.xml` cannot overflow the stack.

Two things make me rank it blocking regardless:

- **The right comparison is the siblings, and three of four get this right.**
  `textpanel.rs:229` handles `Disconnected` explicitly (*"The worker died. The
  built-in face stands."*); `update::Updates::poll` (`update/mod.rs:280`) handles
  it with a comment that names this exact failure — *"The thread ended without
  reporting, which can only be a panic in it. Say so rather than sitting on
  'Checking…' for ever."*; `installwin::poll` handles it. `loading.rs` is the one
  that does not, and it is the one whose stall cannot be dismissed.
- **The consequence is total and unrecoverable**, where every other finding here
  is at worst a process that dies with the same work lost.

Note also that the *same* module reintroduces the risk it is exposed to: it
spawns with `std::thread::spawn` (finding 7), which panics on the main thread if
the OS will not give a thread, where `textpanel` uses `thread::Builder::spawn`
and logs.

### The guard

Two, and both are cheap and need no device:

- `Loading::take` returns an outcome that distinguishes the three cases, and
  `collect_loading` turns `Disconnected` into a notice — *"Umber stopped while
  reading this document"* — and clears `editor.loading`. This is
  `update::Updates::worker_vanished` applied one module over.
- A test that drops the sender without sending and asserts the dialog comes down.
  Constructing a `Loading` with a channel whose sender is dropped is a CPU test:
  it needs no window, no GPU and no file. The panel half — that
  `tabs::loading` is not drawn afterwards — is reachable through
  `egui::Context::run_ui` in the way `docs/audit` guards elsewhere already do.

There is no guard available for "the modal stack is ordered such that the quit
prompt is unreachable" short of measuring egui's own layers; the right fix is to
make the stall impossible rather than to test the wedge.

---

## 2. SUBSTANTIVE — a PNG header alone drives an unbounded zeroed allocation

`crates/umber-core/src/docimport/flat.rs:33–45`:

```rust
decoder.set_limits(png::Limits { bytes: ImportedDocument::MAX_TOTAL_BYTES as usize });
...
let size = reader.output_buffer_size().ok_or_else(|| malformed(...))?;
let mut buf = vec![0u8; size];
let info = reader.next_frame(&mut buf)...;
```

Two things are true of `png` 0.18.1, both read out of the vendored source at
`~/.cargo/registry/src/*/png-0.18.1`:

- `Limits::bytes` bounds only the decoder's **own** allocations. It is spent by
  `reserve_bytes`, which is called for one output *row*
  (`decoder/mod.rs:391`), the ICC profile and a handful of ancillary chunks. It
  never sees the caller's buffer. Umber sets it to 16 GiB, so it is effectively
  off.
- `output_buffer_size` (`decoder/mod.rs:668`) is a pure overflow check: it
  returns `None` only if `width × height × bytes-per-pixel` does not fit
  `isize::MAX`. A 60000 × 60000 RGBA header answers `Some(14_400_000_000)`.

So `vec![0u8; size]` is an allocation the *file header* chooses, from a PNG that
can be a few hundred bytes on disk. In `read_png` the bound that would refuse it
— `check_bounds` — runs at line 94, **after** the decode.

`preview::decode_png` (`preview.rs:252–255`) has the same shape and does not call
`set_limits` at all, so it takes png's 64 MiB default — which, again, bounds
nothing about `vec![0; size]`.

### Why this is a crash and not a slow open

`Vec`'s allocation failure calls `handle_alloc_error`, which **aborts**. An abort
does not run the panic hook, so there is no crash report, no reporter process, no
autosave and no "Your work" section. It is the quietest death in the codebase. On
Linux with overcommit the `alloc_zeroed` may succeed lazily and the process is
then killed by the OOM killer as `next_frame` fills the buffer, which is the same
outcome by a different route.

### Where it is reachable from

- Opening a `.png` in Umber (`flat::read_png`).
- Every layer of an `.ora` (`openraster.rs:560`), a `mergedimage.png`
  (`openraster.rs:841`, `krita.rs:953`), and every saved-history patch
  (`history.rs:243`).
- **Inside Explorer.** `umber-shellext` calls `preview::from_bytes` for an
  `.ora`/`.kra` `mergedimage.png` and a `.clip`'s `CanvasPreview` PNG. `guard`'s
  `catch_unwind` does not catch an abort, and `MAX_DOCUMENT_BYTES` bounds the
  *file*, not the header's claim. A hostile document in a folder somebody browses
  takes down `dllhost.exe`. The module's own rules say this is "everybody's
  problem rather than Umber's", which is precisely why it matters.

`.png` is deliberately not registered on either platform, so the flat-PNG arm is
Umber-only; the container arms are not.

### The guard

Compare `output_buffer_size()` against a stated ceiling **before** allocating,
and refuse with the existing `ImportError::CanvasTooLarge`. For `flat::decode_png`
the ceiling that already exists is `ImportedDocument::MAX_DIMENSION` on each edge
— the decoder hands back `reader.info().size()` before `next_frame`, so the check
is available at the point of the allocation. For `preview::decode_png` the natural
ceiling is smaller still, since the caller immediately calls `fit_within(cx)`.

Testable without provoking an OOM: build a valid IHDR declaring 60000 × 60000 with
no IDAT, assert `decode_png` returns `Err` and assert it did so *before* reading a
frame. The fixture is under a hundred bytes. That is the honest guard — you cannot
ask CI to run out of memory, but you can assert the refusal happens on the header.

---

## 3. SUBSTANTIVE — the per-entry read bound is the *document* bound

`container::read_optional_entry` bounds an entry at
`ImportedDocument::MAX_TOTAL_BYTES` — **16 GiB** — and `read_optional_entry_bounded`
exists precisely because that is the wrong figure for an entry whose size follows
a count rather than a canvas. Its own documentation states the rule:

> A limit measured in gigabytes is the wrong one for a parameter record, and
> effects are the first entry in the archive whose *cardinality* is unbounded by
> the format.

Two callers took the bounded form (`openraster.rs:678` for effects,
`openraster.rs:793` for the text record). Three entries of the identical shape did
not:

| entry | read at | shape |
|---|---|---|
| `umber/…` history manifest | `history.rs:116` | JSON array of entries; count unbounded by the format |
| `stack.xml` | `openraster.rs:155` (`read_entry`) | XML; element count unbounded |
| `maindoc.xml` | `krita.rs:120` (`read_entry`) | XML; element count unbounded |

The declared-size check at `container.rs:92` catches an honest header, and the
`take(limit + 1)` at line 104 exists because *the header is only a claim* — so the
enforcement is `read_to_end` growing the `Vec` to 16 GiB and then being told it is
too large at line 110. The peak is the doubling reallocation on top of that.

The history manifest is the sharpest of the three, because it is then handed to
`serde_json::from_slice` (`history.rs:119`), which materialises the whole entry
list before `manifest.entries.len()` is looked at.

`stack.xml` has a second amplifier of its own: `parse_stack` pushes an
`ImportWarning::GroupFlattened` carrying an owned `String` per nested `<stack>`
past `MAX_DEPTH` (`openraster.rs:390–400`), with no bound on `warnings`. Ten
million nested groups is ten million heap-allocated warnings, which then reach
`tabs::summarise` on the drawing thread.

### The guard

Give each a figure of its own through `read_optional_entry_bounded`, exactly as
effects and the text record already do — a manifest and a `stack.xml` are
kilobytes in every real file. Bound `warnings` while you are there. A test drives
a fixture whose entry deflates past the new figure and asserts the refusal, which
is what `MAX_EFFECTS_BYTES` already has.

---

## 4. SUBSTANTIVE — one PSD entry point has no `catch_unwind`

`photoshop.rs`'s module docs are unambiguous, and they say it was established by
running the crate over its own fixtures:

> It also panics rather than erroring on some real files: ZIP-compressed channel
> data is an `unimplemented!()`, the major-section split slices without bounds
> checks, and `negative-top-left-layer.psd` — a file the crate ships itself —
> panics inside `rgba()`. Parsing therefore runs inside `catch_unwind`.

`photoshop::read` does exactly that (`catch`, `photoshop.rs:296`). The preview
path does not:

```rust
// preview.rs:226
fn psd_composite(bytes: &[u8]) -> Result<Preview, ImportError> {
    let psd = psd::Psd::from_bytes(bytes).map_err(...)?;
    Preview::new(psd.width(), psd.height(), psd.rgba())
}
```

Both named panic sources are here: `from_bytes` (the section split) and `rgba()`.

Consequences by caller:

- **`umber-shellext`** — caught by `guard`, so the file simply has no thumbnail.
  Correct, and only because `panic = "abort"` is not set (finding 6). Note that
  `read_any` tries `Photoshop` for **every** unrecognised byte stream, so this is
  the arm most often reached.
- **`umber-app::thumbnail::run`** (`thumbnail.rs:88`) — not caught. The
  thumbnailer process dies with a panic message on stderr and exit code 101. That
  is *tolerable* by the freedesktop contract, which wants a non-zero exit, and it
  is quiet because `crash::install_hook` is deliberately installed **after** the
  thumbnail branch in `lib.rs:131–151` — so a folder of `.psd` files does not
  produce a crash box per file. That ordering is careful and right.

So the cost today is a stderr panic rather than a clean refusal, and a
`catch_unwind` in `umber-shellext` doing work that belongs one layer down. The
reason to fix it is that the *next* caller of `preview::from_bytes` will not know
to wrap it.

### The guard

Move the `catch` into `preview::psd_composite` — reusing
`photoshop::catch` rather than writing a second one, which is this codebase's own
rule about second implementations. `psd` ships
`negative-top-left-layer.psd`; a fixture reproducing its shape drives the refusal
without a network fetch, and the existing
`an_rle_mask_channel_refuses_the_file_rather_than_taking_the_process_with_it` is
the pattern.

---

## 5. SUBSTANTIVE — `try_reserve`'s "what is still fatal" list is stale both ways

The brief asked whether the enumeration at `canvas.rs:805–831` is complete. It is
not, and it is wrong in the reassuring direction as well as the alarming one.

**Named as unguarded, now guarded.** The list's worst entry reads:

> **An effect slice's page, which is on the frame path.** … `take_whole_page`
> reaches the infallible `ensure_pages` where the pool has none.

`take_whole_page` now calls the **fallible** `try_ensure_pages`
(`canvas.rs:4826`), with its own doc paragraph explaining the change and a
`PageRefusal` type distinguishing a device refusal from the `MAX_SLOTS` ceiling.
The frame-path hazard the list calls "the worst entry" has been closed and the
list still advertises it. That matters because a reader who trusts the list will
spend effort on a fixed problem and skip the next one.

**Not named, and unguarded.** `CanvasRenderer::flip_layers` — Image → Flip, an
ordinary artist command — grows the atlas through the *infallible*
`ensure_pages` (`canvas.rs:6542`):

```rust
if self.layers.free.len() < wanted {
    ...
    self.ensure_pages(device, queue, None, pages);
}
```

and then unconditionally creates a page-sized scratch texture
(`canvas.rs:6546–6559`). Both are `create_texture`, which wgpu-core 29.0.4 maps
through `handle_hal_error_with_nonfatal_oom` (`device/resource.rs:1629`) — so the
device survives, the error is *uncaptured*, `crash::device_error` panics, and the
crash box arrives. On a card with room for the document but not for one more
page plus a scratch, flipping the canvas is the crash box. This is the only
remaining shipped call site of the infallible `ensure_pages`.

For completeness, I verified the rest of the list against wgpu 29.0.4's source
and it is otherwise accurate. `create_buffer`, `create_texture`, `create_sampler`
and `create_query_set` are the only four non-fatal-OOM paths
(`device/resource.rs:1100/1629/2244/4924`); everything else, including
`create_texture_view`, bind groups and `Queue::write_texture`'s staging, calls
the fatal `handle_hal_error`. Two additions the list does not make and could:

- `Device::lose` is also reached from `lose_if_oom`, which wgpu calls after
  **every** `Queue::submit` (`device/queue.rs:1503`) and every `Device::poll`
  (`device/resource.rs:801`). So a device can be lost between two Umber calls
  with no allocation of Umber's involved. Umber sets no
  `device_lost_callback`, so the loss surfaces as the next operation's
  uncaptured error. That is the designed outcome, but it means the frame after a
  driver TDR is the crash box, and nothing distinguishes it from an Umber bug in
  the report.
- `File → New` uses the infallible `Graphics::add_canvas` (`app.rs:4179`,
  `4216`, `5250`) while the import path uses the fallible `make_canvas`
  (`app.rs:4619`). The list does name this; it is worth restating that the tab is
  created *before* the allocation in `create_document`, so there is no
  "a refusal leaves the session as it was" available on that path even if one
  were added.

### The guard

Move `flip_layers` to `try_ensure_pages` and give the flip's scratch the same
treatment `try_reserve` gives the array — a `Vram` back to `app.rs` and
`vram::` sentence. `vram.rs` is honest that its three existing call sites are
"guarded by nothing" and that reachability rests on review; a fourth changes
nothing about that. What *is* testable without a card is the mechanical claim:
`a_reservation_builds_no_view_before_it_has_checked` already scans `try_reserve`'s
source; a sibling that asserts no shipped code calls the infallible
`ensure_pages` would have caught this and would catch the next one.

---

## 6. SUBSTANTIVE — `panic = "abort"` is not set, and something depends on that

`CLAUDE.md`'s Crash reporting section says:

> **`panic = "abort"` changes nothing and there is no `catch_unwind`.**

There is no `panic = "abort"` in the workspace `Cargo.toml`, in any crate
manifest, or in a `.cargo/config.toml` — there is no `.cargo` directory at all.
Umber unwinds.

That is not a nit, because the claim invites somebody to set it, and three things
would break the moment they did:

- **`umber-shellext::guard`** (`lib.rs:98–109`) becomes inert. Every COM entry
  point currently converts a panic — including the `psd` crate's, reached for
  every unrecognised stream by `read_any` — into `E_FAIL`. Under `abort` those
  become an abort inside Explorer's surrogate. The crate's own module docs call
  this out as one of its three structural rules.
- **`docimport::photoshop::catch`** (`photoshop.rs:296–297`) stops refusing the
  file and starts killing the process. The guard
  `an_rle_mask_channel_refuses_the_file_rather_than_taking_the_process_with_it`
  would fail, so that one at least is caught by CI.
- The `CLAUDE.md` paragraph's own reasoning — *"nothing here needs the stack
  unwound"* — is true of the crash hook and false of these two.

The sentence should be inverted: **`panic = "abort"` must not be set**, and the
reason is `umber-shellext`, which is loaded into a process nobody here owns.

### The guard

A test in `umber-shellext` asserting `catch_unwind` actually catches — panic
inside a closure passed to `guard` and assert `E_FAIL`. Under `panic = "abort"`
that test aborts the test binary, which is a loud failure rather than a silent
one. `photoshop.rs`'s existing RLE-mask test already provides half of this
accidentally.

---

## Minor

**7. `Loading::start` spawns with `std::thread::spawn`** (`loading.rs:93`), which
panics if the OS refuses a thread. That panic is on the **main** thread, so it is
the crash box — from clicking Open. `textpanel::start` (`textpanel.rs:183`) uses
`thread::Builder::spawn` and handles `Err` with a log line and a graceful
fallback, for exactly this reason. Guard: match `textpanel`, and surface the
failure as a notice.

**8. `import_reporting` reads the whole file with no bound** (`docimport/mod.rs:144`,
`std::fs::read`). `umber-shellext` bounds the same bytes at `MAX_DOCUMENT_BYTES`
= 1 GiB (`lib.rs:208`) with the reasoning *"A hostile or absurd file must not
take the shell's memory with it."* The editor makes no such bound, so a
multi-gigabyte file is a `Vec` growth that can abort before a single byte is
parsed. This is arguably right for the editor — an artist's real `.clip` can be
307 MB, measured — but the asymmetry is undocumented and the failure is an abort.
Guard: a stated ceiling with a sentence, sized off the same survey the 1 GiB
figure came from.

**9. Unchecked `i64` addition of a tile offset** — `krita.rs:812`,
`(x + offset.0, y + offset.1)`, where both terms come out of the file (the tile
header text and the layer XML). Debug builds panic on overflow; release wraps,
and the wrapped value is then contained by `visible_rect`'s saturating clamps
(`krita.rs:827–842`), which refuse it. So this is a debug-only panic and a
release no-op. `container::crop` had the same shape and was fixed with
`saturating_neg`/`saturating_sub`, with a comment saying the sibling `blit` was
left alone because both its call sites pass `(0, 0)` — I verified that claim and
it holds (`krita.rs:956`, `openraster.rs:844`). This is the third instance of the
same pattern and the one that was not carried forward. Guard: saturate, and
extend `a_layer_that_misses_the_canvas_yields_nothing_at_all`'s `i64::MIN`/`MAX`
sweep to `assemble_tiles`.

**10. `Preview::fit_within`'s unreachable arm violates the invariant it was
given** (`preview.rs:100–108`): when `RgbaImage::from_raw` fails it returns
`Preview { size: (w, h), rgba: Vec::new() }` — a preview claiming a size its
buffer does not have, which is the exact state `Preview::new` exists to refuse
because *"a preview whose buffer is short is an out-of-bounds read in whatever
draws it."* It happens not to be one today: `to_bitmap` zips
`out.chunks_exact_mut(4)` with `preview.rgba.chunks_exact(4)`, so an empty buffer
gives a blank bitmap rather than a read past the end. The comment's instinct —
don't panic inside Explorer — is right; the value is wrong. Guard: return
`Preview::new(1, 1, vec![0; 4])` or thread a `Result`.

---

## Deliberate, and not reported as bugs

I agree with each of these and am not disputing them:

- **`crash::device_error` stays fatal** (`crash/mod.rs:477`). A device that has
  reported an uncaptured error produces undefined results; a quietly wrong canvas
  is worse. Verified that it routes through the ordinary hook rather than
  `wgpu_core`'s own panic.
- **The hook does not report a worker thread** (`crash/mod.rs:346`). Correct in
  general — the application really is still running. Finding 1 is a bug in
  `loading.rs`, not in the hook.
- **`parse_args` ignores an argument it does not recognise.** Refusing to start a
  painting application over a stray word is the worse failure.
- **`REPORTING` latches**, and `gather` uses `try_lock` rather than `lock` because
  the panicking thread may hold it. Both correct, both non-obvious.
- **`swapchain.rs` is right**, and its reasoning about why the model is separable
  from `app.rs`'s translation — with a `debug_assert_eq!` at `app.rs:3624` at the
  one line where the two meet — is the best-defended thing I read in this audit.
- **Host memory has no bound on import**, and `check_resident`'s docs say so
  outright, including that masks are fifteen times understated and that "none of
  them is a *host* bound, and there is not one." Findings 2, 3 and 8 are specific
  amplifiers that make that gap cheap to reach from a small file; the general gap
  is already recorded.
- **`ensure_slots`' `debug_assert` rather than a hard check** — I traced the
  arithmetic (64 layers + 64 masks + 1 float spare + 127 effects = 256 =
  `MAX_SLOTS`) and it is tight but not exceeded.

## What I could not check

- Nothing was built or run; every finding is a reading.
- The two `unsafe` calls in `update::installwin` (`ShellExecuteExW`,
  `WaitForSingleObject`) and `syscursor::hide_now` are unreachable from a test on
  this machine, and `syscursor`'s own docs already say deleting the file breaks
  no test.
- macOS: nothing on that platform has been run by anyone here, and `sysclip`'s
  `TRANSPORT_IS_EXACT` echo path says so. I found no crash risk in it by reading,
  which is not the same as evidence.
- Whether a real out-of-memory or a real device loss behaves as wgpu's source
  says. Provoking either on CI is not reasonable; the guards proposed above all
  stand in for it by asserting the *refusal* happens at the right moment rather
  than by exhausting anything.
